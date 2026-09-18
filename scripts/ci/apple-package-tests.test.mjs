import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, copyFileSync, writeFileSync, readFileSync, rmSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';

for (const failure of ['', 'NanocodexVoice']) {
  test(`two lanes retain all packages and ${failure ? 'propagate failure' : 'succeed'}`, () => {
    const root = mkdtempSync(join(tmpdir(), 'apple-package-ci-'));
    try {
      mkdirSync(join(root, 'scripts/ci'), { recursive: true });
      mkdirSync(join(root, 'bin'));
      copyFileSync(new URL('./apple-package-tests.sh', import.meta.url), join(root, 'scripts/ci/apple-package-tests.sh'));
      writeFileSync(join(root, 'bin/swift'), `#!${process.execPath}
const fs = require('node:fs');
const args = process.argv.slice(2);
const pkg = args[args.indexOf('--package-path') + 1].split('/').pop();
if (!(Number(args[args.indexOf('--jobs') + 1]) >= 1)) process.exit(99);
const log = event => fs.appendFileSync(process.env.EVENTS, JSON.stringify({pkg,event})+'\\n');
log('start');
const deadline = Date.now() + 5000;
const timer = setInterval(() => {
  const started = fs.readFileSync(process.env.EVENTS, 'utf8').trim().split('\\n').map(JSON.parse).filter(e => e.event === 'start').length;
  if (started >= 2) {
    clearInterval(timer);
    setTimeout(() => { log('end'); console.log(pkg + ' full transcript'); process.exit(pkg === process.env.FAIL_PACKAGE ? 7 : 0); }, 50);
  } else if (Date.now() > deadline) process.exit(98);
}, 10);
`, { mode: 0o755 });
      const result = spawnSync('bash', ['scripts/ci/apple-package-tests.sh'], {
        cwd: root, encoding: 'utf8', env: { ...process.env, PATH: `${join(root, 'bin')}:${process.env.PATH}`, EVENTS: join(root, 'events'), FAIL_PACKAGE: failure },
      });
      assert.equal(result.status, failure ? 1 : 0, result.stderr);
      const events = readFileSync(join(root, 'events'), 'utf8').trim().split('\n').map(JSON.parse);
      let active = 0, peak = 0;
      for (const event of events) { active += event.event === 'start' ? 1 : -1; peak = Math.max(peak, active); }
      assert.equal(active, 0);
      assert.equal(peak, 2);
      for (const pkg of ['InboxCore', 'NanocodexVoice', 'NanocodexContext', 'NanocodexHand']) {
        assert.equal(events.filter(event => event.pkg === pkg && event.event === 'start').length, 1);
        assert.ok(existsSync(join(root, `apple/build/evidence/package-tests/${pkg}.log`)));
        assert.ok(result.stdout.includes(`${pkg} full transcript`));
      }
    } finally { rmSync(root, { recursive: true, force: true }); }
  });
}
