#!/usr/bin/env node
import { appendFileSync } from 'node:fs';
import { execFileSync, spawnSync } from 'node:child_process';

const validSha = value => typeof value === 'string' && value.length === 40 && /^[a-f0-9]{40}$/.test(value);

// Run immediately before each production mutation, after expensive preparation.
// Workflow job concurrency serializes deployments; it does not make an older
// checkout current after a newer master push arrived while the job was building.
function currentRelease() {
  if (process.env.GITHUB_EVENT_NAME === 'workflow_dispatch'
    && process.env.DEPLOY_TARGET === 'production') return true;
  if (process.env.GITHUB_EVENT_NAME !== 'push' || process.env.GITHUB_REF !== 'refs/heads/master') {
    throw new Error('Production release guard requires a master push or explicit production dispatch');
  }
  const { GITHUB_REPOSITORY: repository, GITHUB_SHA: sha } = process.env;
  if (typeof repository !== 'string' || /\s/.test(repository)
    || !/^[A-Za-z0-9][A-Za-z0-9-]*\/[A-Za-z0-9_.-]+$/.test(repository) || !validSha(sha)) {
    throw new Error('Production release guard requires a repository and full commit SHA');
  }
  const ref = JSON.parse(execFileSync('gh', ['api', `repos/${repository}/git/ref/heads/master`,
    '--header', 'Cache-Control: no-cache'], { encoding: 'utf8', timeout: 30_000, stdio: ['ignore', 'pipe', 'pipe'] }));
  if (ref?.object?.type !== 'commit' || !validSha(ref?.object?.sha)) {
    throw new Error('GitHub returned an invalid master commit');
  }
  if (ref.object.sha === sha) return true;
  console.log(`::notice::Skipping superseded production push ${sha}; master is ${ref.object.sha}.`);
  return false;
}

try {
  const args = process.argv.slice(2);
  if (args.length && (args[0] !== '--' || args.length < 2)) {
    throw new Error('Usage: current-production-release.mjs [-- command arguments...]');
  }
  const active = currentRelease();
  if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, `active=${active}\n`);
  // A skipped piped mutation (secret bulk) must let its producer finish without
  // EPIPE under Actions' pipefail. Discard stdin without buffering or logging it.
  if (!active && args.length && !process.stdin.isTTY) process.stdin.resume();
  if (active && args.length) {
    const result = spawnSync(args[1], args.slice(2), { stdio: 'inherit' });
    if (result.error || result.signal) throw new Error('Production command could not complete');
    process.exitCode = result.status ?? 1;
  }
} catch {
  // Fail closed without printing subprocess output, tokens, or provider errors.
  console.error('::error::Production release check or command failed; deployment was not authorized or did not complete.');
  process.exitCode = 1;
}
