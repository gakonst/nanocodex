// Live provider state is authoritative after manual deployments and rollbacks.
export const workerScripts = Object.freeze({
  egress: 'nanocodex-egress', x: 'nanocodex-x', media: 'nanocodex-media', managed: 'nanocodex-durable-agent',
  email: 'nanocodex-email', dialog: 'nanocodex-connect-dialog',
  'connect-api': 'nanocodex-connect-api', astra: 'nanocodex-astra-mpp-trial',
  'chief-of-staff': 'nanocodex-chief-of-staff', playground: 'nanocodex-connect-playground',
  account: 'nanocodex',
});
export const accountValid = value => typeof value === 'string' && /^[a-f0-9]{32}$/.test(value) && value.length === 32;
export const providerIdValid = value => typeof value === 'string' && /^[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12}$/.test(value) && value.length === 36;
const failure = () => new Error('Live Worker deployment could not be verified');

export function releaseTag(fingerprint) {
  if (typeof fingerprint !== 'string' || fingerprint.length !== 64 || !/^[a-f0-9]{64}$/.test(fingerprint)) throw failure();
  return `nc-ci-${fingerprint}`;
}

export async function currentWorkerDeployment(worker, {
  account = process.env.CLOUDFLARE_ACCOUNT_ID, token = process.env.CLOUDFLARE_API_TOKEN,
  request = globalThis.fetch,
} = {}) {
  try {
    if (!accountValid(account) || !Object.hasOwn(workerScripts, worker) || typeof token !== 'string' || !token) throw failure();
    const script = workerScripts[worker];
    const base = `https://api.cloudflare.com/client/v4/accounts/${account}/workers/scripts/${script}`;
    const get = async path => {
      const response = await request(`${base}/${path}`, {
        method: 'GET', redirect: 'error', signal: AbortSignal.timeout(30_000),
        headers: { Authorization: `Bearer ${token}`, 'Cache-Control': 'no-cache' },
      });
      if (!response.ok) throw failure();
      const body = await response.json();
      if (body?.success !== true || !body.result) throw failure();
      return body.result;
    };
    // Same ordering and response shape as Wrangler 4.127.1's fetchLatestDeployment.
    const listing = await get('deployments');
    const deployment = Array.isArray(listing.deployments) ? listing.deployments[0] : undefined;
    if (!providerIdValid(deployment?.id) || !Array.isArray(deployment.versions) || deployment.versions.length !== 1) throw failure();
    const traffic = deployment.versions[0];
    if (traffic.percentage !== 100 || !providerIdValid(traffic.version_id)) throw failure();
    const version = await get(`versions/${traffic.version_id}`);
    if (version.id !== traffic.version_id || typeof version.annotations?.['workers/tag'] !== 'string') throw failure();
    return { account, script, deploymentId: deployment.id, versionId: version.id, tag: version.annotations['workers/tag'] };
  } catch {
    // Never expose tokens, provider response bodies, or fetch error causes.
    throw failure();
  }
}
