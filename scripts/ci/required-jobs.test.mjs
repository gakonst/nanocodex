import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';

// Exercise the actual final gate, including GitHub's bash fail-fast semantics.
const workflow = readFileSync(new URL('../../.github/workflows/ci.yml', import.meta.url), 'utf8');
const gate = workflow.split('      - name: Require every applicable CI job\n')[1]
  .split('        run: |\n')[1].split('\n').map(line => line.replace(/^          /, '')).join('\n');
const always = ['CHANGES'];
const selected = { NATIVE_REQUIRED: ['SHARED_HANDS', 'WINDOWS_HAND', 'VM_GUEST'], VOICE_REQUIRED: ['VOICE_NATIVE'], PYTHON_REQUIRED: ['PYTHON'],
  RUST_REQUIRED: ['QUALITY'], WASM_REQUIRED: ['WASM_BUILD'], BINDINGS_REQUIRED: ['BINDINGS'],
  APPS_REQUIRED: ['APPS'], PREVIEW_REQUIRED: ['PREVIEW'], POLICY_REQUIRED: ['POLICY'], CODEQL_REQUIRED: ['CODEQL'] };
function environment(required) {
  return { TEST: 'skipped', WASM_QUALITY: required ? 'success' : 'skipped', ...Object.fromEntries(always.map(key => [key, 'success'])),
    ...Object.fromEntries(Object.entries(selected).flatMap(([key, jobs]) => [[key, String(required)], ...jobs.map(job => [job, required ? 'success' : 'skipped'])])) };
}
const passes = env => spawnSync('bash', ['-e', '-c', gate], { env: { ...process.env, ...env } }).status === 0;

test('full and intentionally reduced matrices pass the real gate', () => {
  assert.ok(passes(environment(true)));
  assert.ok(passes(environment(false)));
  // CUA-only changes keep the native matrix while skipping voice/Python.
  const cua = { ...environment(false), NATIVE_REQUIRED: 'true',
    SHARED_HANDS: 'success', WINDOWS_HAND: 'success', VM_GUEST: 'success' };
  assert.ok(passes(cua));
  for (const job of selected.NATIVE_REQUIRED) {
    for (const result of ['failure', 'cancelled', 'skipped', '']) {
      assert.equal(passes({ ...cua, [job]: result }), false, `CUA ${job}: ${result}`);
    }
  }
});
test('every required check rejects failure, cancellation, and unexpected skips', () => {
  for (const job of [...always, 'WASM_QUALITY', ...Object.values(selected).flat()]) {
    for (const result of ['failure', 'cancelled', 'skipped', '']) {
      assert.equal(passes({ ...environment(true), [job]: result }), false, `${job}: ${result}`);
    }
  }
});
test('missing selection and unplanned execution fail closed', () => {
  for (const key of Object.keys(selected)) {
    for (const value of ['', 'null', 'invalid']) assert.equal(passes({ ...environment(false), [key]: value }), false);
  }
  for (const job of Object.values(selected).flat()) assert.equal(passes({ ...environment(false), [job]: 'success' }), false);
});

test('CUA native selection retains disabled macOS bridge and lifecycle test definitions', () => {
  const sharedHands = workflow.split('  shared-hands:\n')[1].split('  voice-native:\n')[0];
  assert.match(sharedHands, /if: needs\.changes\.outputs\.native == 'true'/);
  assert.match(sharedHands, /os: \[ubuntu-latest, macos-15\]/);
  const bridgeStep = sharedHands.split('      - name: Check CUA bridge and native host lifecycle\n')[1]
    .split('      - name:')[0];
  assert.match(bridgeStep, /if: \$\{\{ false && runner\.os == 'macOS' \}\}/);
  for (const name of ['app-server', 'native-host', 'gui-readiness']) {
    assert.ok(bridgeStep.includes(`scripts/tests/openai-cua-${name}.test.mjs`));
  }
});

test('paused Rust test job must be explicitly skipped', () => {
  for (const result of ['success', 'failure', 'cancelled', '']) {
    assert.equal(passes({ ...environment(true), TEST: result }), false, `TEST: ${result}`);
  }
});

test('affected-family outputs gate producers, consumers and every final prerequisite', () => {
  const jobs = Object.fromEntries([...workflow.matchAll(/^  ([a-z-]+):\n([\s\S]*?)(?=^  [a-z-]+:|$(?![\s\S]))/gm)]
    .map(([, name, body]) => [name, body]));
  for (const [job, family] of Object.entries({ quality: 'rust', policy: 'policy', 'wasm-build': 'wasm',
    bindings: 'bindings', apps: 'apps', 'js-preview': 'preview', codeql: 'codeql' })) {
    assert.ok(jobs[job].includes(`needs.changes.outputs.${family} == 'true'`), job);
    assert.match(jobs.changes, new RegExp(`      ${family}:`));
    assert.ok(jobs['ci-success'].includes(job), `gate dependency ${job}`);
  }
  for (const job of ['bindings', 'apps', 'js-preview']) {
    assert.match(jobs[job], /needs: \[changes, wasm-build\]/);
  }
  assert.match(jobs['wasm-quality'], /needs: changes/);
  assert.ok(jobs['wasm-quality'].includes("if: needs.changes.outputs.rust == 'true' && needs.changes.outputs.bindings == 'true'"));
  assert.match(jobs['wasm-quality'], /cargo clippy --locked --target wasm32-unknown-unknown/);
  assert.ok(!jobs.bindings.includes('cargo clippy'));
  assert.ok(jobs['ci-success'].includes('needs.wasm-quality.result'));
  const bindingSteps = jobs.bindings.split('      - ');
  const install = bindingSteps.find(step => step.includes('name: Install JS consumer dependencies'));
  assert.ok(install.includes('pnpm install --frozen-lockfile'));
  assert.ok(!install.includes('outputs.rust'));
  const policySteps = jobs.policy.split('      - ');
  for (const step of policySteps.filter(step => /cargo-deny|cargo deny|check-(?:experimental|crate|rustls)/.test(step))) {
    assert.ok(step.includes("if: needs.changes.outputs.rust == 'true'"), step);
  }
});

test('Rust quality lanes retain all target and isolated feature checks', () => {
  const quality = workflow.split('  quality:\n')[1].split('  vm-guest:\n')[0];
  assert.match(quality, /fail-fast: false/);
  assert.match(quality, /check: \[workspace-clippy, cli-clippy, contracts, docs\]/);
  const steps = quality.split('      - ');
  const lane = name => steps.filter(step => step.includes(`if: matrix.check == '${name}'`)).join('\n');
  assert.match(lane('workspace-clippy'), /cargo fmt --all -- --check/);
  assert.match(lane('workspace-clippy'), /cargo clippy --locked --workspace --all-targets --all-features --exclude nanocodex-bin/);
  assert.match(lane('cli-clippy'), /--all-features --bin nanocodex --bench tui_render/);
  for (const name of ['nanocodex-oai-api', 'nanocodex-observability', 'nanocodex-tools', 'nanocodex-agent', 'nanocodex-examples']) {
    // Separate invocations preserve each public crate's independent feature resolution.
    assert.ok(lane('contracts').includes(`cargo check --locked --package ${name}`));
  }
  assert.match(lane('docs'), /cargo doc --workspace --all-features --no-deps --locked/);
  assert.match(lane('docs'), /RUSTDOCFLAGS: -D warnings/);
  assert.match(quality, /shared-key: quality-workspace-clippy/);
  assert.ok(passes({ ...environment(true), QUALITY: 'success' }));
  for (const result of ['failure', 'cancelled', 'skipped']) {
    assert.equal(passes({ ...environment(true), QUALITY: result }), false);
  }
});


test('WASM quality is required only for Rust changes with WASM consumers', () => {
  for (const rust of [false, true]) for (const bindings of [false, true]) {
    const env = { ...environment(false), RUST_REQUIRED: String(rust),
      BINDINGS_REQUIRED: String(bindings), QUALITY: rust ? 'success' : 'skipped',
      BINDINGS: bindings ? 'success' : 'skipped', WASM_QUALITY: rust && bindings ? 'success' : 'skipped' };
    assert.ok(passes(env));
    for (const result of ['failure', 'cancelled', '', rust && bindings ? 'skipped' : 'success']) {
      assert.equal(passes({ ...env, WASM_QUALITY: result }), false);
    }
  }
});

test('one Windows runner retains shared native, JS lifecycle, and installer coverage', () => {
  const shared = workflow.split('  shared-hands:\n')[1].split('  voice-native:\n')[0];
  const windows = workflow.split('  windows-hand:\n')[1].split('  quality:\n')[0];
  assert.ok(!shared.includes('windows-latest'));
  assert.match(windows, /runs-on: windows-2025/);
  for (const command of ['-p nanocodex-remote -p nanocodex-tui-control', '-p nanocodex-hand',
    'cargo test --locked --package nanocodex2-bin --bin nanocodex2',
    'device-hand.integration.test.mjs', 'device-hand-shutdown.test.mjs',
    'test-service-build.ps1', 'test-cua-provision.ps1', 'test-setup.ps1',
    'pnpm install --frozen-lockfile', '-Nanocodex2 ./target/debug/nanocodex2.exe']) {
    assert.ok(windows.includes(command), command);
  }
  assert.equal((windows.match(/cargo build --locked/g) ?? []).length, 1);
  assert.ok(!windows.includes('nanocodex-installer-placeholder.exe'));
});
