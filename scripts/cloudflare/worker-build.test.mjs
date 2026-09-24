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
