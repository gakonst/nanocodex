#!/usr/bin/env node
// Wrangler 4.127.1 uses `docker build --load ... -f - <context>` and writes the
// Dockerfile to stdin. WRANGLER_DOCKER_BIN is its supported Docker CLI override.
// Buildx setup alone does not add persistent cache imports/exports to that call.
import { createHash } from 'node:crypto';
import { relative, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

const args = process.argv.slice(2);
let exportedCache;
if (args[0] === 'build') {
  if (!process.env.GITHUB_WORKSPACE || !process.env.BUILDX_BUILDER) {
    throw new Error('Cloudflare image cache requires GITHUB_WORKSPACE and BUILDX_BUILDER');
  }
  // Context separates PhoneContainer and Sandbox, without depending on Wrangler's
  // unique image tags or the runner checkout path. BuildKit keys each layer by
  // its actual Dockerfile, inputs and build arguments; source edits still rebuild.
  const context = relative(process.env.GITHUB_WORKSPACE, resolve(args.at(-1)));
  const scope = `cloudflare-v1-${createHash('sha256').update(context).digest('hex').slice(0, 16)}`;
  const cacheArgs = [];
  if (process.env.GITHUB_REPOSITORY) {
    // Cache-only tags share the existing Hand package without changing runnable
    // image tags or digests. An absent/private cache is a BuildKit cache miss.
    const ref = `ghcr.io/${process.env.GITHUB_REPOSITORY.toLowerCase()}-hand:buildcache-${scope}`;
    cacheArgs.push('--cache-from', `type=registry,ref=${ref}`);
    // Explicit opt-in follows a successful workflow login. Check trust here too
    // so a PR (including pull_request_target) can never enable cache exports.
    if (process.env.WRANGLER_DOCKER_CACHE_WRITE === 'true'
      && process.env.GITHUB_REF === 'refs/heads/master'
      && ['push', 'workflow_dispatch'].includes(process.env.GITHUB_EVENT_NAME)) {
      cacheArgs.push('--cache-to', `type=registry,ref=${ref},mode=max,ignore-error=true`);
      exportedCache = ref;
    }
  }
  args.splice(0, 1, 'buildx', 'build',
    '--builder', process.env.BUILDX_BUILDER, ...cacheArgs);
}
// Preserve stdin (Wrangler's Dockerfile), stdout, stderr, exit status, --load,
// tags, platform and build args. Login, inspect, tag and push pass through.
const result = spawnSync('docker', args, { stdio: 'inherit' });
if (result.error) throw result.error;
if (result.signal) process.kill(process.pid, result.signal);
else {
  process.exitCode = result.status ?? 1;
  if (process.exitCode === 0 && exportedCache) {
    // ignore-error keeps exports optional; make an unseeded cache visible without
    // changing the build result. An existing manifest need not be from this run.
    const cache = spawnSync('docker', ['buildx', 'imagetools', 'inspect', '--raw', exportedCache],
      { stdio: 'ignore', timeout: 30_000 });
    console.log(cache.status === 0
      ? `::notice::Registry cache manifest available: ${exportedCache}`
      : `::notice::Registry cache manifest unavailable: ${exportedCache}. Build succeeded; check cache export logs and GHCR package write access.`);
  }
}
