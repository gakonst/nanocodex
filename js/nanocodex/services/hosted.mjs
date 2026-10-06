/** Describe a private hosted form. Never pass seeds, codes, API keys, or grant tokens. */
export function createHostedRequest(options) {
  const host = new URL(options.host ?? 'https://nanocodex.gakonst.workers.dev');
  if (host.protocol !== 'https:' || host.origin !== host.href.replace(/\/$/, '')) throw new TypeError('Hosted forms require an exact HTTPS origin');
  const appOrigin = options.appOrigin ?? globalThis.location?.origin;
  const origin = appOrigin === undefined ? undefined : new URL(appOrigin);
  if (origin && (origin.origin !== appOrigin || (origin.protocol !== 'https:' && !(origin.protocol === 'http:' && ['localhost', '127.0.0.1', '[::1]'].includes(origin.hostname))))) throw new TypeError('An exact app origin is required');
  const state = options.state ?? globalThis.crypto.randomUUID();
  if (typeof state !== 'string' || state.length < 16 || state.length > 128 || !/^[A-Za-z0-9_-]+$/.test(state)) throw new TypeError('Use a unique state of 16–128 URL-safe characters');
  const service = options.service ?? 'vault';
  if (!['vault', 'phone'].includes(service)) throw new TypeError('Unknown hosted service');
  if (service === 'phone' && !/^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(options.operationId ?? '')) throw new TypeError('Phone approval requires an operation UUID');
  const action = options.action ?? 'enroll';
  const kind = options.kind ?? 'totp';
  if (service === 'vault' && !['login', 'api_key', 'card', 'address', 'phone', 'totp'].includes(kind)) throw new TypeError('Unknown Vault kind');
  if (service === 'vault' && !['enroll', 'select'].includes(action)) throw new TypeError('Unknown Vault action');
  if (service === 'phone' && options.action !== undefined) throw new TypeError('Phone approval does not accept a Vault action');
  const url = new URL(service === 'phone' ? '/services/phone' : '/vault', host);
  if (service === 'phone') url.searchParams.set('operation_id', options.operationId);
  else {
    url.searchParams.set('service', action === 'select' ? 'select' : kind === 'totp' ? 'totp' : 'enroll');
    if (action === 'enroll') { url.searchParams.set('kind', kind); url.searchParams.set('add', kind); }
  }
  if (origin) { url.searchParams.set('enrollment_origin', origin.origin); url.searchParams.set('state', state); }
  return Object.freeze({ url: url.href, origin: host.origin, ...(origin ? { appOrigin: origin.origin } : {}), state, service, ...(service === 'phone' ? { operationId: options.operationId } : { action, kind }) });
}

/** Source window, hosted origin and state must all match. Unknown fields are never forwarded. */
export function readHostedResult(event, request, source) {
  if (!request.appOrigin || !source || event.source !== source || event.origin !== request.origin) return undefined;
  const data = event.data;
  if (!data || data.type !== 'nanocodex:service-enrollment' || data.service !== request.service || data.state !== request.state) return undefined;
  if (request.service === 'phone') {
    if (data.operation_id !== request.operationId || !['complete', 'denied', 'expired', 'failed'].includes(data.status)) return undefined;
    return Object.freeze({ type: data.type, service: 'phone', state: request.state, operation_id: request.operationId, status: data.status });
  }
  if (request.action === 'select') {
    if (data.action !== 'select' || !['api_key', 'login', 'card', 'phone', 'address', 'totp'].includes(data.kind)
      || typeof data.vault_id !== 'string' || !/^[A-Za-z0-9_-]{22,64}$/.test(data.vault_id)
      || typeof data.name !== 'string') return undefined;
    return Object.freeze({ type: data.type, service: 'vault', action: 'select', state: request.state, vault_id: data.vault_id, kind: data.kind, name: data.name });
  }
  if (data.action !== undefined && data.action !== 'enroll') return undefined;
  if (data.service !== 'vault'
    || data.kind !== (request.kind ?? 'totp') || typeof data.vault_id !== 'string' || !/^[A-Za-z0-9_-]{22,64}$/.test(data.vault_id)
    || typeof data.name !== 'string') return undefined;
  if (data.kind === 'totp' || data.origin !== undefined) {
    try { const origin = new URL(data.origin); if (origin.protocol !== 'https:' || origin.origin !== data.origin) return undefined; }
    catch { return undefined; }
  }
  return Object.freeze({ type: data.type, service: 'vault', state: request.state, vault_id: data.vault_id, kind: data.kind, name: data.name, ...(data.origin === undefined ? {} : { origin: data.origin }) });
}

/** Call directly from a click handler so the browser can open the hosted approval window. */
export function openHostedPopup(request, options = {}) {
  if (!request.appOrigin) throw new Error('Hosted popup callbacks require an explicit appOrigin outside a browser');
  const host = options.window ?? globalThis.window;
  if (!host) throw new Error('Hosted enrollment needs a browser window');
  options.signal?.throwIfAborted();
  const popup = host.open(request.url, `nanocodex-service-${request.state}`, 'popup,width=520,height=760');
  if (!popup) throw new Error('The browser blocked the hosted enrollment popup');
  return new Promise((resolve, reject) => {
    let timer;
    const cleanup = () => { host.removeEventListener('message', receive); options.signal?.removeEventListener('abort', abort); clearInterval(timer); popup.close(); };
    const abort = () => { cleanup(); reject(options.signal.reason ?? new Error('Enrollment cancelled')); };
    const receive = event => { const result = readHostedResult(event, request, popup); if (result) { cleanup(); resolve(result); } };
    host.addEventListener('message', receive);
    options.signal?.addEventListener('abort', abort, { once: true });
    timer = setInterval(() => { if (popup.closed) { cleanup(); reject(new Error('Enrollment window closed')); } }, 250);
    if (options.signal?.aborted) abort();
  });
}
