import { pathToFileURL } from 'node:url';

const api = 'https://api.cloudflare.com/client/v4';
const worker = 'nanocodex-connect-api';
const publicConfig = `https://${worker}.gakonst.workers.dev/v1/machine-usd/config`;
const canonicalToken = '0x20c000000000000000000000f37de3740adec032';
const relayHeader = 'x-nanocodex-mach-relay-token';

// Status-only errors: provider bodies, headers, credentials and network errors
// must never enter logs. Each write is attempted exactly once.
class Failure extends Error {}
function requireValue(condition, code) {
  if (!condition) throw new Failure(code);
}
function validConfig(value) {
  return value?.chain_id === 4217 && value?.token_address?.toLowerCase?.() === canonicalToken
    && (value.onramp_enabled === undefined || value.onramp_enabled === true)
    && value.min_usd_amount_cents === 500
    && value.max_usd_amount_cents === 10000
    && typeof value.stripe_publishable_key === 'string' && value.stripe_publishable_key.startsWith('pk_live_') && value.stripe_publishable_key.length > 8;
}

export async function configure(env, transport = fetch, report = console.log) {
  const account = env.CLOUDFLARE_ACCOUNT_ID;
  const cfToken = env.CLOUDFLARE_API_TOKEN;
  const token = env.MACH_ONRAMP_RELAY_TOKEN;
  const host = env.MACH_ONRAMP_RELAY_HOST;
  const origin = env.MACH_ONRAMP_RELAY_URL;
  requireValue(/^[a-f0-9]{32}$/.test(account ?? ''), 'invalid_account');
  requireValue(typeof cfToken === 'string' && cfToken.length > 0, 'missing_cloudflare_token');
  requireValue(typeof token === 'string' && token.length >= 32 && !/[\r\n]/.test(token), 'invalid_relay_token');
  // The environment variable is set from the issuer's verified deployment receipt;
  // workflow input alone can never choose a destination for the credential.
  requireValue(/^[a-z0-9][a-z0-9-]*\.[a-z0-9][a-z0-9-]*\.workers\.dev$/.test(host ?? ''), 'invalid_verified_relay_host');
  requireValue(origin === `https://${host}`, 'relay_origin_not_allowlisted');
  requireValue(['configure', 'configure-origin', 'verify'].includes(env.MACH_ONRAMP_ACTION), 'invalid_action');

  async function request(url, init, stage) {
    let response;
    try {
      response = await transport(url, { ...init, redirect: 'manual', signal: AbortSignal.timeout(15000) });
    } catch {
      throw new Failure(`${stage}_${init.method === 'PUT' ? 'outcome_unknown_do_not_retry' : 'transport_failed'}`);
    }
    report(`${stage}: http_${response.status}`);
    return response;
  }
  async function json(response, stage) {
    try { return await response.json(); }
    catch { throw new Failure(`${stage}_invalid_response`); }
  }
  const auth = { authorization: `Bearer ${cfToken}` };
  const accountApi = `${api}/accounts/${account}/workers`;
  let response = await request(`${accountApi}/subdomain`, { headers: auth }, 'account');
  requireValue(response.status === 200, 'account_unavailable');
  let body = await json(response, 'account');
  requireValue(body.success === true && body.result?.subdomain === 'gakonst', 'wrong_production_account');
  // Confirm the target exists before the secret endpoint can create a new Worker.
  response = await request(`${accountApi}/scripts/${worker}/settings`, { headers: auth }, 'worker');
  requireValue(response.status === 200, 'worker_unavailable');
  body = await json(response, 'worker');
  requireValue(body.success === true && Array.isArray(body.result?.bindings)
    && body.result.bindings.some(binding => binding.name === 'CONNECT_STATE' && binding.type === 'durable_object_namespace'
      && binding.class_name === 'ConnectNonceStorage'
      && (binding.script_name === undefined || binding.script_name === worker)
      && binding.environment === undefined), 'wrong_production_worker');

  if (env.MACH_ONRAMP_ACTION === 'configure-origin') {
    requireValue(!body.result.bindings.some(binding => binding.name === 'MACH_ONRAMP_RELAY_URL'),
      'origin_already_bound_run_verify');
    requireValue(body.result.bindings.some(binding => binding.name === 'MACH_ONRAMP_RELAY_TOKEN'
      && binding.type === 'secret_text'), 'recovery_requires_existing_secret_token');
  }

  response = await request(`${origin}/v1/config`, {}, 'relay_anonymous');
  await response.body?.cancel();
  requireValue(response.status === 401, 'relay_must_require_authentication');
  response = await request(`${origin}/v1/config`, { headers: { [relayHeader]: token } }, 'relay_authenticated');
  requireValue(response.status === 200, 'relay_authentication_failed');
  requireValue(validConfig(await json(response, 'relay_authenticated')), 'relay_config_invalid_or_disabled');

  if (env.MACH_ONRAMP_ACTION !== 'verify') {
    const secrets = env.MACH_ONRAMP_ACTION === 'configure-origin'
      ? [['MACH_ONRAMP_RELAY_URL', origin]]
      : [['MACH_ONRAMP_RELAY_TOKEN', token], ['MACH_ONRAMP_RELAY_URL', origin]];
    for (const [name, text] of secrets) {
      const stage = name === 'MACH_ONRAMP_RELAY_TOKEN' ? 'write_token' : 'write_origin';
      response = await request(`${accountApi}/scripts/${worker}/secrets`, {
        method: 'PUT', headers: { ...auth, 'content-type': 'application/json' },
        body: JSON.stringify({ name, text, type: 'secret_text' }),
      }, stage);
      // Any non-success may follow an applied write. Do not retry automatically.
      requireValue(response.status === 200 || response.status === 201, `${stage}_unconfirmed_do_not_retry`);
      body = await json(response, `${stage}_unconfirmed_do_not_retry`);
      requireValue(body.success === true && body.result?.name === name && body.result?.type === 'secret_text', `${stage}_unconfirmed_do_not_retry`);
    }
  }
  response = await request(publicConfig, {}, 'public_config');
  requireValue(response.status === 200, 'public_config_unavailable_run_verify');
  requireValue(validConfig(await json(response, 'public_config')), 'public_config_invalid_or_disabled');
  report('verified: relay_auth_required canonical_mach_enabled public_config_200');
}

export async function main(env = process.env, transport = fetch, report = console.log) {
  try { await configure(env, transport, report); return 0; }
  catch (error) {
    report(`failed: ${error instanceof Failure ? error.message : 'unexpected_error'}`);
    return 1;
  }
}
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  process.exitCode = await main();
}
