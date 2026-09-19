import specs from './extension-specs.json' with { type: 'json' };

/** Pinned declarations; host transport spells namespace members with __. */
export const extensionSpecs = Object.freeze(specs);
const encoder = new TextEncoder();
const fail = message => { throw new TypeError(message); };
function validate(value, schema, optional = false) {
  if (optional && value == null) return;
  if (schema.oneOf) {
    if (!schema.oneOf.some(branch => { try { validate(value, branch); return true; } catch { return false; } })) fail('invalid variant');
    return;
  }
  if (schema.type === 'object') {
    if (!value || typeof value !== 'object' || Array.isArray(value)) fail('expected object');
    for (const key of schema.required ?? []) if (!(key in value)) fail(`missing ${key}`);
    for (const [key, field] of Object.entries(value)) {
      if (!schema.properties[key]) { if (schema.additionalProperties === false) fail(`unknown field ${key}`); }
      else validate(field, schema.properties[key], !(schema.required ?? []).includes(key));
    }
  } else if (schema.type === 'array') {
    if (!Array.isArray(value) || value.length < (schema.minItems ?? 0)) fail('invalid array');
    value.forEach(item => validate(item, schema.items));
  } else if (schema.type === 'integer') {
    if (!Number.isInteger(value) || value >= 2 ** 64 || value < (schema.minimum ?? 0)) fail('invalid integer');
  } else if (typeof value !== schema.type) fail(`expected ${schema.type}`);
  if (schema.enum && !schema.enum.includes(value)) fail('invalid enum');
}
export function validateExtensionInput(name, input) {
  const spec = specs.find(spec => spec.name === name);
  if (!spec) fail('unknown extension tool');
  validate(input, spec.parameters);
  return input;
}
function relativePath(path = '') {
  if (typeof path !== 'string' || path.includes('\0') || path.includes('\\') || path.startsWith('/') || /^[a-z]:/i.test(path)) fail('path must stay within the memories root');
  const parts = path.split('/').filter(part => part && part !== '.');
  if (parts.some(part => part.startsWith('.'))) fail('path is hidden or outside the memories root');
  return parts.join('/');
}
function page(items, cursor, max) {
  if (cursor != null && !/^\d+$/.test(cursor)) fail('cursor must be a non-negative integer');
  const start = Number(cursor ?? 0);
  if (!Number.isSafeInteger(start) || start > items.length) fail('cursor exceeds result count');
  const end = Math.min(start + max, items.length);
  return { values: items.slice(start, end), next_cursor: end < items.length ? String(end) : null, truncated: end < items.length };
}
/** The adapter is bound to ONE authorized private store by the host, never selected by model input.
 * listFiles returns visible relative regular-file paths. readFile must reject symlinks.
 * createFile must be atomic create-if-absent, with no overwrites.
 */
export function fileMemoriesBackend(store) {
  async function files(path) {
    const scope = relativePath(path ?? '');
    const all = [...await store.listFiles()].map(relativePath).sort();
    if (scope && !all.some(file => file === scope || file.startsWith(scope + '/')) && !(await store.listDirectories?.() ?? []).includes(scope)) fail('path was not found');
    return all.filter(file => !scope || file === scope || file.startsWith(scope + '/'));
  }
  return {
    async list(input) {
      validateExtensionInput('memories__list', input);
      const scope = relativePath(input.path ?? '');
      const entries = new Map();
      for (const directory of await store.listDirectories?.() ?? []) {
        const path = relativePath(directory);
        const tail = scope ? (path.startsWith(scope + '/') ? path.slice(scope.length + 1) : '') : path;
        if (tail) entries.set((scope ? scope + '/' : '') + tail.split('/')[0], 'directory');
      }
      for (const file of await files(input.path)) {
        if (file === scope) entries.set(file, 'file');
        else {
          const tail = file.slice(scope ? scope.length + 1 : 0);
          const first = tail.split('/')[0];
          entries.set((scope ? scope + '/' : '') + first, tail.includes('/') ? 'directory' : 'file');
        }
      }
      const result = page([...entries].sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0).map(([path, entry_type]) => ({ path, entry_type })), input.cursor, Math.max(1, Math.min(input.max_results ?? 2000, 2000)));
      return { path: input.path ?? null, entries: result.values, next_cursor: result.next_cursor, truncated: result.truncated };
    },
    async read(input) {
      validateExtensionInput('memories__read', input);
      const original = await store.readFile(relativePath(input.path));
      const startLine = input.line_offset ?? 1;
      if (!startLine || input.max_lines === 0) fail('line_offset and max_lines must be positive');
      let start = 0;
      for (let line = 1; line < startLine; line++) {
        const newline = original.indexOf('\n', start);
        if (newline < 0) fail('line_offset exceeds file length');
        start = newline + 1;
      }
      let end = original.length;
      if (input.max_lines != null) {
        let offset = start;
        for (let line = 0; line < input.max_lines; line++) {
          const newline = original.indexOf('\n', offset);
          if (newline < 0) break;
          offset = newline + 1;
          if (line + 1 === input.max_lines) end = offset;
        }
      }
      const selected = original.slice(start, end);
      const content = truncateMemoryText(selected, 20000);
      return { path: input.path, start_line_number: startLine, content, truncated: end < original.length || selected !== content };
    },
    async search(input) {
      validateExtensionInput('memories__search', input);
      const queries = input.queries.map(query => query.trim());
      const prepare = text => {
        if (!(input.case_sensitive ?? true)) text = text.toLowerCase();
        return input.normalized ? text.replace(/[^\p{L}\p{N}]/gu, '') : text;
      };
      const prepared = queries.map(prepare);
      if (!prepared.length || prepared.some(query => !query)) fail('queries must not be empty or contain empty strings');
      const match_mode = input.match_mode ?? { type: 'any' };
      if (match_mode.type === 'all_within_lines' && !match_mode.line_count) fail('line_count must be positive');
      const matches = [];
      for (const path of await files(input.path)) {
        const text = await store.readFile(path);
        const lines = text === '' ? [] : text.replace(/\n$/, '').split('\n').map(line => line.replace(/\r$/, ''));
        const flags = lines.map(line => prepared.map(query => prepare(line).includes(query)));
        const windows = [];
        for (let start = 0; start < lines.length; start++) {
          if (!flags[start].some(Boolean)) continue;
          const window = match_mode.type === 'all_within_lines' ? match_mode.line_count : 1;
          const found = queries.map(() => false);
          for (let end = start; end < Math.min(lines.length, start + window); end++) {
            flags[end].forEach((flag, i) => { found[i] ||= flag; });
            if (match_mode.type === 'any' ? found.some(Boolean) : found.every(Boolean)) { windows.push({ start, end, found }); break; }
          }
        }
        for (const window of windows) {
          if (windows.some(other => other !== window && window.start <= other.start && window.end >= other.end)) continue;
          const begin = Math.max(0, window.start - (input.context_lines ?? 0));
          const end = Math.min(lines.length, window.end + (input.context_lines ?? 0) + 1);
          matches.push({ path, match_line_number: window.start + 1, content_start_line_number: begin + 1,
            content: lines.slice(begin, end).join('\n'), matched_queries: queries.filter((_, i) => window.found[i]) });
        }
      }
      const result = page(matches, input.cursor, Math.max(1, Math.min(input.max_results ?? 200, 200)));
      return { queries, match_mode, path: input.path ?? null, matches: result.values, next_cursor: result.next_cursor, truncated: result.truncated };
    },
    async add_ad_hoc_note(input) {
      validateExtensionInput('memories__add_ad_hoc_note', input);
      // Upstream runtime permits a leading hyphen even though its schema does not.
      if (!/^\d{4}-\d{2}-\d{2}T\d{2}-\d{2}-\d{2}-[a-z0-9-]{1,80}\.md$/.test(input.filename) || encoder.encode(input.filename).length > 128) fail('invalid ad-hoc note filename');
      if (!input.note.trim()) fail('ad-hoc note must not be empty');
      await store.createFile('extensions/ad_hoc/notes/' + input.filename, input.note);
      return {};
    },
  };
}
/** Same byte-based 4 bytes/token policy as Codex's text budget approximation. */
export function truncateMemoryText(text, tokens) {
  const bytes = encoder.encode(text);
  const budget = tokens * 4;
  if (bytes.length <= budget) return text;
  let left = Math.floor(budget / 2), right = bytes.length - (budget - left);
  while (left > 0 && (bytes[left] & 0xc0) === 0x80) left--;
  while (right < bytes.length && (bytes[right] & 0xc0) === 0x80) right++;
  const decoder = new TextDecoder();
  return decoder.decode(bytes.subarray(0, left)) + `…${Math.ceil((bytes.length - budget) / 4)} tokens truncated…` + decoder.decode(bytes.subarray(right));
}
/** No store means no declaration. The host authorizes every call. */
export function extensionTools({ memories, authorize }) {
  return specs.flatMap(spec => {
    const method = spec.name.split('__')[1];
    if (!memories || typeof memories[method] !== 'function') return [];
    return [{ ...spec, handler: async (input, context) => {
      await authorize(spec.name, context);
      context.signal.throwIfAborted();
      validateExtensionInput(spec.name, input);
      return memories[method](input, context);
    } }];
  });
}
