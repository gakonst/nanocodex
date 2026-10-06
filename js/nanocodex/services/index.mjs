import { createAccountServices } from './account.mjs';
/** Standalone account services. This entry point has no agent or WASM dependencies. */
export { createHostedRequest, readHostedResult, openHostedPopup } from './hosted.mjs';
export { normalizeServices, serviceResource } from './scope.mjs';

export class ServiceError extends Error {
  constructor(message, { status, code, outcomeUnknown = false } = {}) {
    super(message);
    this.name = 'ServiceError';
    this.status = status;
    this.code = code;
    this.outcomeUnknown = outcomeUnknown;
  }
}

/** Every mutation is dispatched at most once. Retain phone operation IDs for reconciliation. */
export function createServicesClient(options) {
  if (!options || typeof options !== 'object') throw new TypeError('Service client options are required');
  const connected = options.connect !== undefined;
  if (connected === (options.apiKey !== undefined)) throw new TypeError('Choose an API key or a Connect client');
  const base = new URL(options.baseUrl ?? 'https://nanocodex.gakonst.workers.dev');
  if (base.protocol !== 'https:' && !(base.protocol === 'http:' && ['localhost', '127.0.0.1', '[::1]'].includes(base.hostname))) {
    throw new TypeError('Service API requires HTTPS');
  }
  if (base.username || base.password || base.search || base.hash) throw new TypeError('Invalid service API URL');
  if (!connected && (typeof options.apiKey !== 'string' || !options.apiKey.trim())) throw new TypeError('API key is required');
  if (connected && typeof options.connect.fetch !== 'function') throw new TypeError('Connect client must expose fetch');
  const prefix = connected ? `/v1/grants/${segment(options.grantId)}/services` : '/v1/services';
  const fetcher = options.fetch ?? globalThis.fetch;
  async function request(path, method = 'GET', body, controls = {}) {
    const headers = { accept: 'application/json' };
    if (body !== undefined) headers['content-type'] = 'application/json';
    if (!connected) headers.authorization = `Bearer ${options.apiKey}`;
    if (connected && options.connect.appOrigin) headers.origin = options.connect.appOrigin;
    let response;
    let value;
    try {
      const init = { method, headers, redirect: 'manual', credentials: 'omit', signal: controls.signal,
        ...(body === undefined ? {} : { body: JSON.stringify(body) }) };
      response = connected
        ? await options.connect.fetch(prefix + path, init)
        : await fetcher(new URL(prefix + path, base), init);
      if ((response.status >= 300 && response.status < 400) || response.type === 'opaqueredirect') {
        throw new Error('Service API redirects are not followed');
      }
      value = response.status === 204 ? undefined : await boundedAccountJson(response);
    } catch {
      throw new ServiceError(method === 'GET' ? 'Service request failed' : 'Service operation outcome is unknown; inspect its status before taking further action', {
        code: method === 'GET' ? 'transport_error' : 'outcome_unknown', outcomeUnknown: method !== 'GET',
      });
    }
    if (!response.ok) {
      const candidate = typeof value?.error === 'string' ? value.error : value?.error?.code;
      const code = ['unauthorized', 'forbidden', 'not_found', 'invalid_request', 'human_approval_required', 'phone_operation_outcome_unknown', 'phone_service_unavailable', 'vault_unavailable', 'outcome_unknown'].includes(candidate) ? candidate : 'service_request_failed';
      throw new ServiceError(typeof code === 'string' ? code : `Service request failed (${response.status})`, {
        status: response.status, code, outcomeUnknown: method !== 'GET' && (response.status >= 500 || /outcome_unknown/.test(code ?? '')),
      });
    }
    return value;
  }
  const account = createAccountServices(async (path, method, body, controls = {}) => {
    if (connected) throw new ServiceError('Account access required', { code: 'account_only' });
    // Serialization is completed before dispatch. Never include caller values in diagnostics.
    let serialized;
    try {
      serialized = body === undefined ? undefined : JSON.stringify(body);
      if (serialized !== undefined && new TextEncoder().encode(serialized).byteLength > 64 * 1024) throw new Error();
    } catch {
      throw new ServiceError('Invalid account request body', { code: 'invalid_request' });
    }
    const controller = new AbortController();
    const abort = () => controller.abort();
    if (controls.signal?.aborted) abort();
    controls.signal?.addEventListener('abort', abort, { once: true });
    const timer = setTimeout(abort, 30_000);
    let response;
    try {
      response = await fetcher(new URL(path, base), {
        method, headers: { accept: 'application/json', authorization: `Bearer ${options.apiKey}`,
          ...(serialized === undefined ? {} : { 'content-type': 'application/json' }) },
        redirect: 'manual', credentials: 'omit', signal: controller.signal,
        ...(serialized === undefined ? {} : { body: serialized }),
      });
      if (!response.ok || response.type === 'opaqueredirect') {
        await response.body?.cancel();
        const unknown = method !== 'GET' && (response.status === 0 || response.status >= 500 || response.status === 408 || response.status >= 300 && response.status < 400);
        throw new ServiceError('Account request failed', {
          status: response.status, code: unknown ? 'outcome_unknown' : 'account_request_failed', outcomeUnknown: unknown,
        });
      }
      if (response.status === 204) return undefined;
      if (!/^application\/json(?:\s*;|$)/i.test(response.headers.get('content-type') ?? '')) throw new Error();
      return await boundedAccountJson(response);
    } catch (error) {
      if (error instanceof ServiceError) throw error;
      throw new ServiceError('Account request failed', {
        status: response?.status, code: method === 'GET' ? 'account_request_failed' : 'outcome_unknown', outcomeUnknown: method !== 'GET',
      });
    } finally {
      clearTimeout(timer);
      controls.signal?.removeEventListener('abort', abort);
    }
  });
  const hostedKeys=['service','action','kind','operation_id','app_origin','state'];
  return Object.freeze({
    account,
    links: (query, controls) => account.links(query, controls),
    hosted: async (query = {}, controls) => {
      const params = new URLSearchParams();
      for (const [k, v] of Object.entries(query)) { if (v === undefined) continue; if (!hostedKeys.includes(k) || typeof v !== 'string' || !v) throw new TypeError('Invalid hosted link query'); params.set(k, v); }
      if (connected) throw new ServiceError('Account access required', { code: 'account_only' });
      const r = await request('/links' + (params.size ? '?' + params : ''), 'GET', undefined, controls);
      if (typeof r?.url !== 'string' || typeof r?.origin !== 'string' || typeof r?.state !== 'string') throw new ServiceError('Invalid hosted link response', { code: 'invalid_response' });
      const link = new URL(r.url);
      if (link.protocol !== 'https:' || link.origin !== r.origin || link.username || link.password) throw new ServiceError('Invalid hosted URL', { code: 'invalid_response' });
      return Object.freeze(Object.fromEntries(['url','origin','appOrigin','state','service','action','kind','operationId'].filter(k => typeof r[k] === 'string').map(k => [k, r[k]])));
    },
    catalog: (controls) => request('', 'GET', undefined, controls),
    vault: Object.freeze({
      list: async (controls) => ({ vault: (await request('/vault', 'GET', undefined, controls)).vault.map(vaultMetadata) }),
      get: async (id, controls) => ({ entry: vaultMetadata((await request(`/vault/${segment(id)}`, 'GET', undefined, controls)).entry) }),
      request: async (input, controls) => {
        const result = await request('/vault/request', 'POST', input, controls);
        if (!Number.isInteger(result?.status) || result.status < 100 || result.status > 599 || result.ok !== (result.status >= 200 && result.status < 300)) {
          throw new ServiceError('Invalid broker receipt; request outcome is unknown', { code: 'outcome_unknown', outcomeUnknown: true });
        }
        return Object.freeze({ status: result.status, ok: result.ok });
      },
    }),
    phone: Object.freeze({
      available: (query, controls) => request('/phone/numbers/available' + queryString(query), 'GET', undefined, controls),
      list: (controls) => request('/phone/numbers', 'GET', undefined, controls),
      provision: (input, controls) => request('/phone/numbers', 'POST', operation(input), controls),
      get: (id, controls) => request(`/phone/numbers/${segment(id)}`, 'GET', undefined, controls),
      release: (id, input, controls) => request(`/phone/numbers/${segment(id)}`, 'DELETE', operation(input), controls),
      messages: (id, query, controls) => request(`/phone/numbers/${segment(id)}/messages${queryString(query)}`, 'GET', undefined, controls),
      requests: Object.freeze({
        get: (id, controls) => request(`/phone/requests/${segment(id)}`, 'GET', undefined, controls),
      }),
    }),
  });
}

function segment(value) {
  if (typeof value !== 'string' || !/^[A-Za-z0-9_-]{1,128}$/.test(value)) throw new TypeError('An opaque service ID is required');
  return encodeURIComponent(value);
}
function operation(input) {
  if (!input || !/^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(input.operation_id)) {
    throw new TypeError('A stable UUID operation_id is required');
  }
  return input;
}
function queryString(query) {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(query ?? {})) if (value !== undefined) params.set(key, String(value));
  return params.size ? '?' + params : '';
}
function vaultMetadata(entry) {
  const fields = { api_key: [], login: ['username', 'browser_origin'], card: ['last4'],
    address: ['address_line_1', 'address_line_2', 'city', 'state', 'zip', 'country'], phone: ['phone_number'],
    totp: ['issuer', 'account', 'origin', 'algorithm', 'digits', 'period'] };
  if (!entry || !Object.hasOwn(fields, entry.kind) || typeof entry.id !== 'string' || typeof entry.name !== 'string') {
    throw new ServiceError('Invalid Vault metadata', { code: 'invalid_response' });
  }
  return Object.freeze(Object.fromEntries(['id', 'kind', 'name', 'created_at', ...fields[entry.kind]]
    .filter(key => entry[key] !== undefined).map(key => [key, entry[key]])));
}

/** Bound bytes while streaming, including responses without a Content-Length header. */
async function boundedAccountJson(response) {
  const reader = response.body?.getReader();
  if (!reader) throw new Error();
  const decoder = new TextDecoder('utf-8', { fatal: true });
  let bytes = 0;
  let text = '';
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      bytes += value.byteLength;
      if (bytes > 1024 * 1024) throw new Error();
      text += decoder.decode(value, { stream: true });
    }
    return JSON.parse(text + decoder.decode());
  } catch {
    await reader.cancel().catch(() => {});
    throw new Error('Invalid account response');
  } finally {
    reader.releaseLock();
  }
}
