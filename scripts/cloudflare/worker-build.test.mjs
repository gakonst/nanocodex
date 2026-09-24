import { test } from 'node:test';
import assert from 'node:assert/strict';
import { cpSync, mkdtempSync, mkdirSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { outputs, transfer } from './worker-build.mjs';

const revision = 'a'.repeat(40), runId = '123';
function fixture(fn) {
  const root = mkdtempSync(join(tmpdir(), 'worker-build-'));
  const source = join(root, 'source'), destination = join(root, 'destination');
  mkdirSync(source); mkdirSync(destination);
  const files = outputs.map(path => path.endsWith('.js') ? path : `${path}/fixture.js`);
  for (const path of files) {
    mkdirSync(dirname(join(source, path)), { recursive: true });
    writeFileSync(join(source, path), `built ${path}`);
  }
  const pack = () => {
    transfer('pack', { cwd: source, revision, runId });
    cpSync(join(source, '.ci-worker-build'), join(destination, '.ci-worker-build'), { recursive: true });
  };
  try { fn({ source, destination, files, pack }); }
  finally { rmSync(root, { recursive: true, force: true }); }
}

test('same-run Worker outputs restore into a separate checkout without source or credentials', () => fixture(({ source, destination, files, pack }) => {
  writeFileSync(join(source, '.dev.vars'), 'PRIVATE=fixture');
  writeFileSync(join(source, 'Cargo.toml'), 'source');
  pack();
  transfer('restore', { cwd: destination, revision, runId });
  for (const path of files) assert.equal(readFileSync(join(destination, path), 'utf8'), `built ${path}`);
  for (const path of ['.dev.vars', 'Cargo.toml']) assert.throws(() => readFileSync(join(destination, path)), { code: 'ENOENT' });
}));

test('missing outputs and symlinks fail before artifact publication', () => fixture(({ source, files, pack }) => {
  rmSync(join(source, files[0]));
  assert.throws(pack, /empty build output/);
  symlinkSync('/etc/passwd', join(source, files[0]));
  assert.throws(pack, /must not be a symlink/);
}));

test('wrong revision, run, output list, and archive corruption fail closed', () => fixture(({ destination, files, pack }) => {
  pack();
  assert.throws(() => transfer('restore', { cwd: destination, revision: 'b'.repeat(40), runId }), /does not match checkout/);
  assert.throws(() => transfer('restore', { cwd: destination, revision, runId: '456' }), /another workflow run/);
  const manifest = join(destination, '.ci-worker-build/manifest.json');
  const receipt = JSON.parse(readFileSync(manifest, 'utf8'));
  writeFileSync(manifest, JSON.stringify({ ...receipt, outputs: ['Cargo.toml'] }));
  assert.throws(() => transfer('restore', { cwd: destination, revision, runId }), /unexpected Worker/);
  writeFileSync(manifest, JSON.stringify(receipt));
  writeFileSync(join(destination, '.ci-worker-build/outputs.tar.gz'), 'corrupt');
  assert.throws(() => transfer('restore', { cwd: destination, revision, runId }), /archive is corrupt/);
  assert.throws(() => readFileSync(join(destination, files[0])), { code: 'ENOENT' });
}));

test('production and Worker readiness never depend on container builds', () => {
  const workflow = readFileSync(new URL('../../.github/workflows/cloudflare.yml', import.meta.url), 'utf8');
  const job = name => workflow.split('\n  ' + name + ':\n')[1].split(/\n  [\w-]+:\n/)[0];
  const production = job('production');
  assert.doesNotMatch(production, /needs:|needs\.image-plan|needs\.managed-images/);
  assert.match(production, /uses: \.\/\.github\/actions\/deploy-workers/);
  assert.match(job('preview'), /needs: worker-build/);
  assert.doesNotMatch(job('worker-build'), /\n    needs:|secrets\.|CLOUDFLARE_API_TOKEN|environment:|github.event_name == 'push'/);
  assert.match(job('preview'), /node scripts\/cloudflare\/worker-build\.mjs restore/);
  assert.doesNotMatch(production, /worker-build\.mjs|cloudflare-worker-build/);
  const action = readFileSync(new URL('../../.github/actions/deploy-workers/action.yml', import.meta.url), 'utf8');
  for (const command of ['release-plan.mjs plan', 'release-plan.mjs install', 'release-workers.mjs', 'save-wasm-outputs']) assert.ok(action.includes(command), 'missing command: ' + command);
  assert.ok(action.indexOf('release-plan.mjs plan') < action.indexOf('release-plan.mjs install'));
  assert.ok(action.indexOf('release-plan.mjs install') < action.indexOf('release-workers.mjs'));
  assert.doesNotMatch(action, /release-plan\.mjs build/);
  assert.ok(action.indexOf('release-workers.mjs') < action.indexOf('save-wasm-outputs'));
  assert.match(action, /!cancelled\(\) && steps\.release\.outputs\.wasm-built == 'true'/);
});
