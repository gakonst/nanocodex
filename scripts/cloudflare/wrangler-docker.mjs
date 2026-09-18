#!/usr/bin/env node
// Wrangler 4.127.1 uses `docker build --load ... -f - <context>` and writes the
// Dockerfile to stdin. WRANGLER_DOCKER_BIN is its supported Docker CLI override.
// Buildx setup alone does not add persistent cache imports/exports to that call.
import { createHash } from 'node:crypto';
import { relative, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

const args = process.argv.slice(2);
if (args[0] === 'build') {
  if (!process.env.GITHUB_WORKSPACE || !process.env.BUILDX_BUILDER) {
    throw new Error('Cloudflare image cache requires GITHUB_WORKSPACE and BUILDX_BUILDER');
  }
  // Context separates PhoneContainer and Sandbox, without depending on Wrangler's
  // unique image tags or the runner checkout path. BuildKit keys each layer by
  // its actual Dockerfile, inputs and build arguments; source edits still rebuild.
  const context = relative(process.env.GITHUB_WORKSPACE, resolve(args.at(-1)));
  const scope = `cloudflare-v1-${createHash('sha256').update(context).digest('hex').slice(0, 16)}`;
  args.splice(0, 1, 'buildx', 'build',
    '--builder', process.env.BUILDX_BUILDER,
    '--cache-from', `type=gha,version=2,scope=${scope}`,
    '--cache-to', `type=gha,version=2,scope=${scope},mode=max,timeout=3m,ignore-error=true`);
}
// Preserve stdin (Wrangler's Dockerfile), stdout, stderr, exit status, --load,
// tags, platform and build args. Login, inspect, tag and push pass through.
const result = spawnSync('docker', args, { stdio: 'inherit' });
if (result.error) throw result.error;
if (result.signal) process.kill(process.pid, result.signal);
else process.exitCode = result.status ?? 1;
