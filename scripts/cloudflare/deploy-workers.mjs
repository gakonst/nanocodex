#!/usr/bin/env node
// Deploy independent Workers concurrently within each existing dependency phase.
// Account deployment stays in its own final workflow step.
import { spawn } from 'node:child_process';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const phases = {
  infrastructure: [
    ['egress', 'js/egress', ['npx', 'wrangler', 'deploy', '--config', 'wrangler.broker.jsonc']],
    ['x', 'js/x-api', ['npx', 'wrangler', 'deploy', '--env=']],
    ['media', 'js/media', ['npx', 'wrangler', 'deploy', '--config', 'wrangler.jsonc']],
  ],
  consumers: [
    ['email', 'js/email', ['npx', 'wrangler', 'deploy', '--env=']],
    ['dialog', 'js/connect-dialog', ['npx', 'wrangler', 'deploy', '--config', 'wrangler.jsonc']],
    ['connect-api', 'js/connect-api', ['npx', 'wrangler', 'deploy', '--config', 'wrangler.jsonc']],
    ['astra', 'examples/astra-mpp-trial', ['npx', 'wrangler', 'deploy', '--env=']],
    ['chief-of-staff', 'js/chief-of-staff', ['npx', 'wrangler', 'deploy', '--config', 'wrangler.jsonc']],
    ['playground', 'js/connect-playground', ['npx', 'wrangler', 'deploy', '--config', 'wrangler.jsonc']],
  ],
};

export async function deployPhase(phase, { cwd = process.cwd(), env = process.env, launch = spawn } = {}) {
  if (!Object.hasOwn(phases, phase)) throw new Error(`Unknown deployment phase: ${phase}`);
  const guard = resolve(cwd, 'scripts/cloudflare/current-production-release.mjs');
  const results = await Promise.all(phases[phase].map(([name, directory, command]) => new Promise(done => {
    console.log(`Deploying ${name}`);
    // Every mutation retains the current-master guard. Its outputs belong only
    // to the individual command, not the concurrent parent workflow step.
    const childEnv = { ...env };
    delete childEnv.GITHUB_OUTPUT;
    const child = launch(process.execPath, [guard, '--', ...command, '--message', env.DEPLOY_MESSAGE], {
      cwd: resolve(cwd, directory), env: childEnv, stdio: 'inherit',
    });
    child.once('error', () => done({ name, ok: false }));
    child.once('close', code => done({ name, ok: code === 0 }));
  })));
  const failed = results.filter(result => !result.ok).map(result => result.name);
  if (failed.length) throw new Error(`Worker deployments failed: ${failed.join(', ')}`);
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  await deployPhase(process.argv[2]);
}
