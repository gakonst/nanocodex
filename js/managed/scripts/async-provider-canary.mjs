/**
 * Real ChatGPT subscription compatibility canary, using remote Wrangler preview.
 *
 * Run with Node >=22:
 *   node js/managed/scripts/async-provider-canary.mjs CONFIG_JSON OUTPUT_DIRECTORY [http|ws|ws-incremental]
 *
 * CONFIG_JSON supplies the operator's existing Cloudflare account_id, vars.OWNER
 * and vars.TEAM, a NANOCODEX service binding to nanocodex-egress, and a USERS
 * Durable Object binding to UserAccount in nanocodex-durable-agent. Keep private
 * account config in ignored output/. Wrangler must already be authenticated.
 * Set WRANGLER_MODULE only when using an existing installation outside this tree.
 *
 * The worker verifies the owner/team and active ChatGPT credential metadata, then
 * uses the existing browser-model subject binding. It does not register subjects,
 * create threads, modify credentials, or deploy a production Worker. Missing
 * existing authority is a failure. Only synthetic provider output is retained.
 */
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const [configArg, outputArg, transportArg] = process.argv.slice(2);
if (transportArg && !['http', 'ws', 'ws-incremental'].includes(transportArg)) throw new Error('Invalid canary transport');
if (!configArg || !outputArg) throw new Error('Usage: async-provider-canary.mjs CONFIG_JSON OUTPUT_DIRECTORY');
const config = path.resolve(configArg);
const output = path.resolve(outputArg);
fs.mkdirSync(output, { recursive: true });
const stdout = process.stdout.write.bind(process.stdout);
// Wrangler can log preview URLs and control-plane details: suppress its output.
process.stdout.write = () => true;
process.stderr.write = () => true;
process.env.WRANGLER_LOG = 'none';
process.env.WRANGLER_SEND_METRICS = 'false';
const require = createRequire(import.meta.url);
const { unstable_dev } = require(process.env.WRANGLER_MODULE || 'wrangler');
function record(value) {
  const line = JSON.stringify({ at: new Date().toISOString(), ...value });
  fs.appendFileSync(path.join(output, 'results.jsonl'), `${line}\n`);
  stdout(`${line}\n`);
}
let preview;
try {
  record({ stage: 'preview_start' });
  preview = await unstable_dev(fileURLToPath(new URL('./async-provider-canary-worker.mjs', import.meta.url)), {
    config, envFiles: [], persist: false, local: false, logLevel: 'none', ip: '127.0.0.1', port: 0,
    experimental: { disableExperimentalWarning: true, disableDevRegistry: true, watch: false, showInteractiveDevSession: false },
  });
  record({ stage: 'preview_ready' });
  for (const transport of (transportArg ? [transportArg] : ['http', 'ws', 'ws-incremental'])) {
    record({ stage: 'probe_start', transport });
    const response = await preview.fetch(`/${transport}`, { method: 'POST' });
    const result = await response.json();
    record({ stage: 'probe_result', preview_status: response.status, result });
    if (!response.ok || !result.uptake || result.terminal !== 'response.completed') process.exitCode = 1;
  }
} catch (error) {
  record({ stage: 'preview_error', name: error.name, code: typeof error.code === 'number' ? error.code : null });
  process.exitCode = 1;
} finally {
  if (preview) { await preview.stop(); record({ stage: 'preview_stopped' }); }
}
