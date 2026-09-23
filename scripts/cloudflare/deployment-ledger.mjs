import { spawn } from 'node:child_process';

const fingerprintValid = value => typeof value === 'string' && /^[a-f0-9]{64}$/.test(value) && value.length === 64;
const idValid = value => Number.isSafeInteger(value) && value > 0;
const failure = () => new Error('Deployment ledger request failed; release state is uncertain');

// Bodies go through stdin, never shell arguments. Do not surface subprocess
// output or error causes: provider diagnostics can contain private information.
export async function ghRequest({ method, path, body }, launch = spawn) {
  const args = ['api', path, '--method', method, '--header', 'Accept: application/vnd.github+json',
    '--header', 'X-GitHub-Api-Version: 2022-11-28', '--header', 'Cache-Control: no-cache'];
  if (body !== undefined) args.push('--input', '-');
  return new Promise((resolve, reject) => {
    let output = '';
    const child = launch('gh', args, { stdio: ['pipe', 'pipe', 'ignore'] });
    const timer = setTimeout(() => { child.kill(); reject(failure()); }, 30_000);
    child.once('error', () => { clearTimeout(timer); reject(failure()); });
    child.stdout.on('data', chunk => {
      output += chunk;
      if (output.length > 1024 * 1024) { child.kill(); reject(failure()); }
    });
    child.stdin.on('error', () => {}); // close handles failed/uncertain writes
    child.stdin.end(body === undefined ? undefined : JSON.stringify(body));
    child.once('close', code => {
      clearTimeout(timer);
      if (code !== 0) { reject(failure()); return; }
      try { resolve(JSON.parse(output)); } catch { reject(failure()); }
    });
  });
}

export function deploymentEnvironment(worker) {
  if (typeof worker !== 'string' || worker.length > 100 || !/^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(worker)
    || /\s/.test(worker)) throw new Error('Invalid deployment component');
  return `nanocodex-production-${worker}`;
}

// Requires deployments:write for releases (includes reads), deployments:read
// for selection alone. These environments are separate from the workflow's
// production approval environment. Production job concurrency must serialize
// releases, including explicit rollbacks, across the entire selection/mutation.
// API contract: https://docs.github.com/en/rest/deployments/deployments
// https://docs.github.com/en/rest/deployments/statuses
export function createDeploymentLedger({ repository = process.env.GITHUB_REPOSITORY,
  ref = process.env.GITHUB_SHA, request = ghRequest } = {}) {
  if (typeof repository !== 'string' || /\s/.test(repository)
    || !/^[A-Za-z0-9][A-Za-z0-9-]*\/[A-Za-z0-9_.-]+$/.test(repository)) {
    throw new Error('Deployment ledger requires a repository');
  }
  const base = `repos/${repository}/deployments`;
  const active = new WeakSet();
  const call = async args => {
    try { return await request(args); } catch { throw failure(); }
  };
  const status = async (record, state) => {
    const result = await call({ method: 'POST', path: `${base}/${record.id}/statuses`,
      body: { state, environment: record.environment, auto_inactive: false } });
    if (!idValid(result?.id) || result.state !== state || result.environment !== record.environment) throw failure();
  };
  return {
    async lastSuccessfulFingerprint(worker) {
      const environment = deploymentEnvironment(worker);
      try {
        // Never search history for a matching success: a rollback or interrupted
        // newer attempt invalidates any older successful fingerprint.
        const deployments = await call({ method: 'GET', path: `${base}?environment=${encodeURIComponent(environment)}&per_page=1` });
        if (!Array.isArray(deployments) || deployments.length !== 1) return null;
        const latest = deployments[0];
        if (!idValid(latest?.id) || latest.environment !== environment || latest.production_environment !== true) return null;
        let payload = latest.payload;
        if (typeof payload === 'string') payload = JSON.parse(payload);
        if (payload?.schema !== 1 || !fingerprintValid(payload.fingerprint)) return null;
        const statuses = await call({ method: 'GET', path: `${base}/${latest.id}/statuses?per_page=1` });
        if (!Array.isArray(statuses) || statuses.length !== 1 || !idValid(statuses[0]?.id)
          || statuses[0].state !== 'success' || statuses[0].environment !== environment) return null;
        return payload.fingerprint;
      } catch { return null; }
    },
    async start(worker, fingerprint) {
      const environment = deploymentEnvironment(worker);
      if (!fingerprintValid(fingerprint)) throw new Error('Deployment ledger requires a SHA-256 fingerprint');
      if (typeof ref !== 'string' || ref.length !== 40 || !/^[a-f0-9]{40}$/.test(ref)) {
        throw new Error('Deployment ledger requires a full commit SHA');
      }
      const result = await call({ method: 'POST', path: base, body: {
        ref, environment, payload: { schema: 1, fingerprint }, auto_merge: false,
        required_contexts: [], production_environment: true, transient_environment: false,
      } });
      if (!idValid(result?.id) || result.environment !== environment || result.sha !== ref) throw failure();
      const record = Object.freeze({ id: result.id, environment });
      // Caller must await this before ANY external mutation. If interrupted or
      // status creation fails, the newest deployment remains unsafe to skip.
      await status(record, 'in_progress');
      active.add(record);
      return record;
    },
    async finish(record, state) {
      // Caller may report success ONLY after the actual command ran (not a
      // superseded/skipped guard) and exited successfully. Known command failure
      // reports failure; interruption leaves in_progress. No automatic retries.
      if (!record || !active.has(record) || !['success', 'failure', 'inactive'].includes(state)) {
        throw new Error('Invalid deployment ledger completion');
      }
      active.delete(record);
      await status(record, state);
    },
  };
}
