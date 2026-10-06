const prefix = 'urn:nanocodex:services:';
const object = value => value && typeof value === 'object' && !Array.isArray(value);
function exact(value, keys) {
  if (!object(value) || Object.keys(value).some(key => !keys.includes(key))) throw new TypeError('Invalid service capabilities');
}
function ids(value) {
  if (!Array.isArray(value) || value.length > 64 || value.some(id => typeof id !== 'string' || !/^[A-Za-z0-9_-]{1,128}$/.test(id)) || new Set(value).size !== value.length) throw new TypeError('Invalid service IDs');
  return Object.freeze([...value].sort());
}
export function normalizeServices(value) {
  if (value === undefined) return undefined;
  exact(value, ['vault', 'phone']);
  const result = {};
  if (value.vault !== undefined) {
    const v = value.vault;
    exact(v, ['ids', 'origins', 'request']);
    if (typeof v.request !== 'boolean' || !Array.isArray(v.origins) || v.origins.length > 64 || new Set(v.origins).size !== v.origins.length) throw new TypeError('Invalid Vault scope');
    for (const origin of v.origins) {
      const url = new URL(origin);
      if (url.protocol !== 'https:' || url.origin !== origin || url.username || url.password) throw new TypeError('Vault scope requires exact HTTPS origins');
    }
    result.vault = Object.freeze({ ids: ids(v.ids), origins: Object.freeze([...v.origins].sort()), request: v.request });
  }
  if (value.phone !== undefined) {
    const p = value.phone;
    exact(p, ['numberIds', 'read', 'provision', 'release']);
    if (['read', 'provision', 'release'].some(key => typeof p[key] !== 'boolean')) throw new TypeError('Invalid phone scope');
    result.phone = Object.freeze({ numberIds: ids(p.numberIds), read: p.read, provision: p.provision, release: p.release });
  }
  return Object.freeze(result);
}
export function serviceResource(value) {
  const normalized = normalizeServices(value);
  if (normalized === undefined) throw new TypeError('Service capabilities are required');
  return prefix + encodeURIComponent(JSON.stringify(normalized));
}
