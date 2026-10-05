// Provider credentials stay in host-owned closures, outside enumerable descriptors.
const backends = new WeakMap();
export const nativeClaudeDefaults = Symbol('nanocodex.backend.claude.defaults');
export const ownedBackendTools = Symbol('nanocodex.backend.tools');

export function codex(options) { return make('codex', options, ['apiKey', 'apiBaseUrl', 'websocketUrl', 'websocketWarmup']); }
export function claude(options) { return make('claude', options, ['apiKey', 'endpoint']); }

function make(kind, options, keys) {
  if (!options || typeof options !== 'object' || Array.isArray(options)) throw new TypeError('Backend options must be an object');
  for (const key of Object.keys(options)) if (!keys.includes(key)) throw new TypeError(`unsupported Backend.${kind} option: ${key}`);
  if (typeof options.apiKey !== 'string' || !options.apiKey.trim()) throw new TypeError('Backend apiKey must be a non-empty string');
  for (const key of ['endpoint', 'apiBaseUrl', 'websocketUrl']) {
    if (options[key] === undefined) continue;
    let url;
    try { url = new URL(options[key]); } catch { throw new TypeError(`Backend ${key} must be an absolute URL`); }
    const protocols = key === 'websocketUrl' ? ['ws:', 'wss:'] : ['http:', 'https:'];
    if (typeof options[key] !== 'string' || !protocols.includes(url.protocol) || url.username || url.password || url.hash) throw new TypeError(`Backend ${key} must be an absolute URL without credentials or fragment`);
  }
  if (options.websocketWarmup !== undefined && typeof options.websocketWarmup !== 'boolean') throw new TypeError('Backend websocketWarmup must be boolean');
  const backend = Object.freeze({ kind });
  backends.set(backend, Object.freeze({ ...options }));
  return backend;
}

export function resolveBackend(backend) {
  const options = backend && backends.get(backend);
  if (!options) throw new TypeError('backend must be created by Backend.codex() or Backend.claude()');
  return { kind: backend.kind, ...options };
}
