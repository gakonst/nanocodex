import { test } from 'node:test';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { fileURLToPath } from 'node:url';
const run = promisify(execFile);
const root = fileURLToPath(new URL('../../../', import.meta.url));
test('large fresh receipt survives fragmented transport, exact-key replay and subsequent work', { timeout: 600_000 }, async () => {
  const build = await run('cargo', ['test', '-p', 'nanocodex-managed', '--test', 'it', '--no-run', '--message-format=json'], { cwd: root, maxBuffer: 8 * 1024 * 1024 });
  const executable = build.stdout.split('\n').filter(Boolean).map(line => JSON.parse(line))
    .find(value => value.reason === 'compiler-artifact' && value.target.name === 'it' && value.executable)?.executable;
  if (!executable) throw new Error('SDK journey executable missing');
  const label = `large-receipt-${Date.now()}`;
  console.log(`Evidence: output/managed-api-ttft/${label}`);
  const result = await run(process.execPath, [fileURLToPath(new URL('../benchmark/curl-ttft.mjs', import.meta.url)), '--mode=stream', '--samples=1', '--process-samples=0', `--label=${label}`, `--receipt-sdk=${executable}`], { cwd: root, maxBuffer: 8 * 1024 * 1024 });
  console.log(result.stdout);
});
