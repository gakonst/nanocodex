/** Owner-only management. OAuth authorization URLs are completed by the browser. */
export function createAccountServices(request) {
  const call = (path, method = 'GET', body, controls) =>
    request('/v1' + path, method, body, controls).then(project);
  const login = (provider) => Object.freeze({
    start: (input = {}, controls) => call(`/credentials/${provider}/login`, 'POST', input, controls),
    status: (controls) => call(`/credentials/${provider}/login`, 'GET', undefined, controls),
    disconnect: (controls) => call(`/credentials/${provider}`, 'DELETE', undefined, controls),
    ...(provider === 'claude' ? {
      complete: (input, controls) => call('/credentials/claude/login/complete', 'POST', input, controls),
    } : {}),
  });
  return Object.freeze({
    links: (query, controls) => call('/account/links' + linkQuery(query), 'GET', undefined, controls).then(linkResult),
    connectors: Object.freeze({
      catalog: (controls) => call('/connectors/catalog', 'GET', undefined, controls),
      list: (controls) => call('/connectors', 'GET', undefined, controls),
      start: (provider, input = {}, controls) => call(`/connectors/${oauthProvider(provider)}`, 'POST', input, controls),
      disconnect: (provider, id, controls) => call(`/connectors/${connectorProvider(provider)}/connections/${opaque(id, 43, 43)}`, 'DELETE', undefined, controls),
      cloudflare: (input, controls) => {
        opaque(input?.vault_id, 22, 64);
        return call('/connectors/cloudflare', 'POST', input, controls);
      },
      link: Object.freeze({
        status: (attempt, controls) => call(`/connectors/link?attempt=${opaque(attempt, 43, 43)}`, 'GET', undefined, controls),
      }),
      whatsapp: Object.freeze({
        start: (input, controls) => {
          uuid(input?.operation_id);
          if (!/^\+[1-9][0-9]{6,14}$/.test(input?.phone)) throw new TypeError('E.164 phone required');
          return call('/connectors/whatsapp/start', 'POST', input, controls);
        },
        status: (controls) => call('/connectors/whatsapp', 'GET', undefined, controls),
      }),
      mcp: Object.freeze({
        list: (controls) => call('/connectors/mcp-connections', 'GET', undefined, controls),
        create: (input, controls) => call('/connectors/mcp-connections', 'POST', input, controls),
        start: (id, input = {}, controls) => call(`/connectors/mcp-connections/${opaque(id, 43, 43)}/start`, 'POST', input, controls),
        disconnect: (id, controls) => call(`/connectors/mcp-connections/${opaque(id, 43, 43)}`, 'DELETE', undefined, controls),
      }),
    }),
    credentials: Object.freeze({
      overview: (controls) => call('/credentials', 'GET', undefined, controls),
      chatgpt: login('chatgpt'),
      claude: login('claude'),
      openai: Object.freeze({
        save: (input, controls) => call('/credentials/openai', 'PUT', input, controls),
        delete: (controls) => call('/credentials/openai', 'DELETE', undefined, controls),
      }),
    }),
    vault: Object.freeze({
      list: (controls) => call('/services/vault', 'GET', undefined, controls),
      get: (id, controls) => call(`/services/vault/${opaque(id, 22, 64)}`, 'GET', undefined, controls),
      create: (kind, input, controls) => call(`/credentials/vault/${vaultKind(kind)}`, 'POST', input, controls),
      delete: (kind, id, controls) => call(`/credentials/vault/${vaultKind(kind)}/${opaque(id, 22, 64)}`, 'DELETE', undefined, controls),
      loginOrigin: (id, input, controls) => call(`/credentials/vault/login/${opaque(id, 22, 64)}/origin`, 'PUT', input, controls),
      ssh: Object.freeze({
        put: (reference, input, controls) => call(`/credentials/ssh/${sshReference(reference)}`, 'PUT', input, controls),
        remove: (reference, controls) => call(`/credentials/ssh/${sshReference(reference)}`, 'DELETE', undefined, controls),
      }),
      store: (input, controls) => {
        uuid(input?.operation_id);
        opaque(input?.capture_id, 22, 64);
        if (input.address_vault_id !== undefined) opaque(input.address_vault_id, 22, 64);
        return call('/vault/store', 'POST', input, controls);
      },
      card: (input, controls) => {
        if (!['status', 'balance', 'refresh'].includes(input?.operation)
          || (input.capture_id === undefined) === (input.vault_id === undefined)) throw new TypeError('Invalid card operation');
        opaque(input.capture_id ?? input.vault_id, 22, 64);
        if (input.operation === 'refresh' || input.operation_id !== undefined) uuid(input.operation_id);
        return call('/vault/card', 'POST', input, controls);
      },
    }),
  });
}

function opaque(value, min, max) {
  if (typeof value !== 'string' || !new RegExp(`^[A-Za-z0-9_-]{${min},${max}}$`).test(value)) throw new TypeError('Invalid account ID');
  return value;
}
function sshReference(value) {
  if (typeof value !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/.test(value)) throw new TypeError('Invalid SSH reference');
  return value;
}
function uuid(value) {
  if (typeof value !== 'string' || !/^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(value)) throw new TypeError('Stable UUID required');
}
const kinds = ['login', 'api_key', 'card', 'address', 'phone', 'totp'];
const oauthProviders = ['google', 'github', 'slack', 'x', 'spotify', 'soundcloud', 'link'];
const providers = [...oauthProviders, 'cloudflare', 'whatsapp'];
function vaultKind(value) {
  if (!kinds.includes(value)) throw new TypeError('Invalid Vault kind');
  return value;
}
function connectorProvider(value) {
  if (!providers.includes(value)) throw new TypeError('Invalid connector provider');
  return value;
}
function oauthProvider(value) {
  if (!oauthProviders.includes(value)) throw new TypeError('Use the dedicated connector start method');
  return value;
}

// Keep only documented public metadata; never expose secret fields or arbitrary nested objects.
const strings = new Set(`active accountId id kind name label title description provider status phase reference hostname username host_key_sha256 public_key email account_id connection_id vault_id capture_id operation_id last4 browser_origin address_line_1 address_line_2 city state zip country phone_number issuer account origin algorithm authorization_url verification_url verification_uri verification_uri_complete user_code currency freshness transport type`.split(' '));
const numbers = new Set('port digits period interval balance observed_at provider_updated_at amount retry_after poll_after_ms limited_until'.split(' '));
const booleans = new Set('connected configured saved deleted ok ready active'.split(' '));
const dates = new Set('created_at updated_at expires_at'.split(' '));
const lists = new Set('capabilities models model_ids supported_models reasoning_efforts scopes'.split(' '));
const nested = new Set('accounts login attempt wallet links access providers connectors connections mcp_connections mcp_connection vault entry ssh chatgpt claude openai google gmail gdrive gcalendar gtasks gdocs gsheets gslides gcontacts github slack x spotify soundcloud cloudflare link whatsapp'.split(' '));
function project(value, depth = 0) {
  if (value === undefined || value === null) return value;
  if (depth > 16 || typeof value !== 'object') throw new TypeError('Invalid account response');
  if (Array.isArray(value)) return Object.freeze(value.map(item => project(item, depth + 1)));
  const result = {};
  for (const [key, item] of Object.entries(value)) {
    if (['authorization_url', 'verification_url', 'verification_uri', 'verification_uri_complete'].includes(key) && item !== undefined) {
      const url = new URL(item);
      if (url.protocol !== 'https:' || url.username || url.password) throw new TypeError('Invalid authorization URL');
    }
    if ((strings.has(key) || key === 'default_model' || key === 'provider_id' || key === 'attempt_id') && typeof item === 'string'
      || numbers.has(key) && typeof item === 'number' && Number.isFinite(item)
      || booleans.has(key) && typeof item === 'boolean'
      || dates.has(key) && (typeof item === 'string' || typeof item === 'number' && Number.isFinite(item))) result[key] = item;
    else if (lists.has(key) && Array.isArray(item)) result[key] = Object.freeze(item.filter(entry => typeof entry === 'string' || entry && typeof entry === 'object').map(entry => typeof entry === 'string' ? entry : project(entry, depth + 1)));
    else if (nested.has(key) && item !== null && typeof item === 'object') result[key] = project(item, depth + 1);
    else if (['attempt', 'connections', 'vault', 'wallet', 'access'].includes(key) && typeof item === 'string') result[key] = item;
  }
  return Object.freeze(result);
}
function linkQuery(query) {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(query ?? {})) {
    if (value === undefined) continue;
    if (key === 'connect' && [...providers, 'claude', 'chatgpt', 'openai', 'mcp'].includes(value)
      || key === 'add' && kinds.includes(value)) params.set(key, value);
    else throw new TypeError('Invalid account link query');
  }
  return params.size ? '?' + params : '';
}
function linkResult(value) {
  const result = {};
  for (const key of ['connections', 'vault', 'wallet', 'access']) {
    if (typeof value?.[key] !== 'string') throw new TypeError('Invalid account links response');
    const url = new URL(value[key]);
    if (url.protocol !== 'https:' || url.username || url.password) throw new TypeError('Invalid account link URL');
    result[key] = url.href;
  }
  return Object.freeze(result);
}
