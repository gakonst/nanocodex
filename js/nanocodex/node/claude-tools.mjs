import { spawn } from 'node:child_process';
import { createRequire } from 'node:module';
import { randomUUID } from 'node:crypto';
import { chmod, lstat, open as openFile, opendir, realpath, rename, unlink } from 'node:fs/promises';
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { open as openWorkspace } from './workspace.mjs';

// Node counterpart of nanocodex-claude-tools. Its filesystem adapters are not
// compiled into WASM and have no native Node binding. This keeps Claude's tool
// protocol separate from the Codex tool registry. A workspace is a path scope,
// NOT an OS sandbox; Bash executes with the embedding user's permissions.
const MAX_FILE = 1024 * 1024;
const MAX_OUTPUT = 64 * 1024;
const string = { type: 'string' };
const boolean = { type: 'boolean' };
const integer = (minimum, maximum, fallback) => ({ type: 'integer', minimum,
  ...(maximum === undefined ? {} : { maximum }), ...(fallback === undefined ? {} : { default: fallback }) });
const enumeration = (...values) => ({ type: 'string', enum: values });
const schema = (properties, required = []) => ({ type: 'object', properties,
  ...(required.length ? { required } : {}), additionalProperties: false });
const definitions = [
  ['Read', 'Read a UTF-8 workspace file with numbered lines.', schema({ file_path: string, offset: integer(1), limit: integer(1) }, ['file_path'])],
  ['Edit', 'Replace exact text in a workspace file, requiring one occurrence unless replace_all is true.', schema({ file_path: string, old_string: string, new_string: string, replace_all: boolean }, ['file_path', 'old_string', 'new_string'])],
  ['Write', 'Atomically replace a UTF-8 workspace file, creating parent directories as needed.', schema({ file_path: string, content: string }, ['file_path', 'content'])],
  ['Glob', 'List matching workspace files using *, ? and ** wildcards.', schema({ pattern: string, path: string }, ['pattern'])],
  ['Grep', 'Search UTF-8 files with a bounded Rust regex embedded in WASM. Default output is matching file paths; glob supports only *, ? and **. Unsupported type filters are rejected.', schema({ pattern: string, path: string, glob: string, output_mode: { ...enumeration('content', 'files_with_matches', 'count'), default: 'files_with_matches' }, '-B': integer(0, 2000), '-A': integer(0, 2000), '-C': integer(0, 2000), context: integer(0, 2000), '-n': { ...boolean, default: true }, '-i': boolean, '-o': boolean, head_limit: integer(0, undefined, 250), offset: integer(0, undefined, 0), multiline: boolean }, ['pattern'])],
  ['Bash', 'Run a foreground Bash command in the authorized native host workspace. This is not an OS sandbox. Background sessions and sandbox bypass flags are unavailable.', schema({ command: { ...string, minLength: 1, maxLength: 16384 }, description: { ...string, maxLength: 1024 }, timeout: integer(1, 600000, 120000), run_in_background: { ...boolean, description: 'Only false is supported; no background session capability is installed.' }, dangerouslyDisableSandbox: { ...boolean, description: 'Unsupported; this flag is always rejected.' } }, ['command'])],
  ['NotebookEdit', 'Replace, insert, or delete a cell in an existing Jupyter notebook. Inserts are after cell_id, or at the beginning when omitted; replacements and deletes require cell_id.', schema({ notebook_path: string, new_source: string, cell_id: string, cell_type: enumeration('code', 'markdown'), edit_mode: { ...enumeration('replace', 'insert', 'delete'), default: 'replace' } }, ['notebook_path', 'new_source'])],
];

/** Install native Claude tools for an explicitly supplied native directory. */
export async function createClaudeTools({ workspace, module }) {
  if (typeof workspace !== 'string' || !workspace) throw new TypeError('Claude workspace must be a non-empty native path');
  const alias = resolve(workspace);
  const root = await realpath(alias);
  if (!(await lstat(root)).isDirectory()) throw new Error('workspace root is not a directory');
  const files = await openWorkspace({ path: root });
  const lifetime = new AbortController();
  const running = new Set();
  let closed = false;
  let closing;
  let matcherClass;
  async function loadMatcher() {
    return matcherClass ??= (async () => {
      if (module === undefined) return createRequire(import.meta.url)('../pkg-node/nanocodex.js').ClaudeGrepRegex;
      const wasm = await import('../pkg-web/nanocodex.js');
      const { initializeBrowserEngine } = await import('../browser/engine.mjs');
      await initializeBrowserEngine({ module });
      return wasm.ClaudeGrepRegex;
    })();
  }
  // Like createNodeProcessTools, do not inherit credentials or shell startup hooks.
  const env = Object.fromEntries(['PATH', 'HOME', 'USER', 'TMPDIR', 'LANG', 'LC_ALL', 'SYSTEMROOT']
    .filter(key => process.env[key] !== undefined).map(key => [key, process.env[key]]));
  env.TERM = 'dumb';

  function pathOf(text, allowRoot = false) {
    if (typeof text !== 'string' || bytes(text) > 4096 || text.includes('\0')) throw new Error('invalid workspace path');
    let path = text;
    if (isAbsolute(path)) {
      const prefix = [root, alias].find(value => path === value || path.startsWith(value.endsWith(sep) ? value : value + sep));
      if (!prefix) throw new Error('absolute path outside workspace');
      path = path.slice(prefix.length).replace(/^[/\\]/, '');
    }
    const parts = path.split(sep).filter(Boolean);
    if (parts.some(part => part === '..' || part === '.') && !(allowRoot && text === '.')) {
      throw new Error('path must not contain traversal, root, or special components');
    }
    if (allowRoot && (path === '.' || !path)) return '';
    if (!parts.length) throw new Error('file path is empty');
    return parts.join('/');
  }
  async function existing(text, requireFile = false) {
    const rel = pathOf(text, true);
    const path = await realpath(join(root, rel));
    const inside = relative(root, path);
    if (inside === '..' || inside.startsWith(`..${sep}`) || isAbsolute(inside)) throw new Error('symlink escapes workspace');
    if (requireFile && !(await lstat(path)).isFile()) throw new Error('path is not a regular file');
    return { rel, path };
  }
  async function readText(path) {
    const handle = await openFile(path, 'r');
    try {
      if (!(await handle.stat()).isFile()) throw new Error('path is not a regular file');
      const buffer = Buffer.alloc(MAX_FILE + 1);
      let size = 0;
      while (size < buffer.length) {
        const read = await handle.read(buffer, size, buffer.length - size, null);
        if (!read.bytesRead) break;
        size += read.bytesRead;
      }
      if (size > MAX_FILE) throw new Error('file exceeds 1 MiB text limit');
      try { return new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(buffer.subarray(0, size)); }
      catch { throw new Error('file is not UTF-8 text'); }
    } finally { await handle.close(); }
  }
  async function safeTarget(rel) {
    let path = root;
    for (const segment of rel.split('/')) {
      path = join(path, segment);
      try {
        const info = await lstat(path);
        if (info.isSymbolicLink()) throw new Error('symlink target or parent rejected');
        if (path !== join(root, rel) && !info.isDirectory()) throw new Error('parent is not a directory');
        if (path === join(root, rel) && !info.isFile()) throw new Error('target is not a regular file');
      } catch (error) { if (error.code !== 'ENOENT') throw error; }
    }
  }
  async function writeText(rel, content, signal, before) {
    if (bytes(content) > MAX_FILE) throw new Error('content exceeds 1 MiB text limit');
    await safeTarget(rel);
    signal.throwIfAborted();
    // Workspace owns directory checks and temporary-file creation. A second
    // same-directory rename lets us preserve the existing mode before publishing.
    const temp = join(dirname(rel), `.nanocodex-claude-${randomUUID()}.tmp`).split(sep).join('/');
    try {
      await files.writeFile(temp, content);
      const handle = await openFile(join(root, temp), 'r');
      try { await handle.sync(); } finally { await handle.close(); }
      await safeTarget(rel);
      try { await chmod(join(root, temp), (await lstat(join(root, rel))).mode); }
      catch (error) { if (error.code !== 'ENOENT') throw error; }
      if (before !== undefined && await readText(join(root, rel)) !== before) throw new Error('file changed during edit');
      signal.throwIfAborted();
      await rename(join(root, temp), join(root, rel));
    } finally { await unlink(join(root, temp)).catch(error => { if (error.code !== 'ENOENT') throw error; }); }
  }
  async function walk(path, signal) {
    const stack = [path];
    const found = [];
    let visits = 0;
    while (stack.length) {
      signal.throwIfAborted();
      if (++visits > 10000) throw new Error('search exceeds 10000 entries');
      const next = stack.pop();
      const info = await lstat(next);
      if (info.isSymbolicLink()) continue;
      if (info.isFile()) found.push(next);
      else if (info.isDirectory()) {
        for await (const entry of await opendir(next)) {
          if (visits + stack.length >= 10000) throw new Error('search exceeds 10000 entries');
          stack.push(join(next, entry.name));
        }
      }
    }
    return found.sort();
  }
  const shown = path => relative(root, path).split(sep).join('/');

  // A dedicated transport is necessary: Node process tools combine stdout and
  // stderr and expose sessions, whereas Claude Bash returns two bounded streams
  // after foreground completion. No arbitrary host callback is installed.
  async function processRun(executable, args, { signal, timeout, cap = 4096 }) {
    signal.throwIfAborted();
    if (closed) throw new Error('Claude tools are closed');
    const child = spawn(executable, args, { cwd: root, env, detached: process.platform !== 'win32', stdio: ['pipe', 'pipe', 'pipe'] });
    const captured = [[], []];
    const sizes = [0, 0];
    let truncated = false;
    let failure;
    let exitCode;
    let resolveExit;
    const exited = new Promise(resolve => { resolveExit = resolve; });
    const kill = signalName => {
      if (!child.pid) return;
      try { process.kill(process.platform === 'win32' ? child.pid : -child.pid, signalName); }
      catch (error) { if (error.code !== 'ESRCH') throw error; }
    };
    // Darwin can briefly refuse a group signal while Node reaps the shell.
    // Match the bounded retry used by createNodeProcessTools.
    const stopGroup = async signalName => {
      for (let attempt = 0; ; attempt++) {
        try { kill(signalName); return; }
        catch (error) {
          if (process.platform !== 'darwin' || error.code !== 'EPERM' || attempt === 4) throw error;
          await delay(25);
        }
      }
    };
    let stopping;
    const stop = () => stopping ??= (async () => {
      await stopGroup('SIGTERM');
      await Promise.race([exited, delay(100, undefined, { ref: false })]);
      await stopGroup('SIGKILL');
      await exited;
    })();
    const abort = () => { failure ??= signal.reason ?? new Error('command cancelled'); void stop().catch(error => { failure = error; }); };
    const timer = setTimeout(() => { failure ??= new Error(`command timed out after ${timeout} milliseconds`); void stop().catch(error => { failure = error; }); }, timeout);
    signal.addEventListener('abort', abort, { once: true });
    if (signal.aborted) abort();
    child.stdin.on('error', error => { if (error.code !== 'EPIPE') failure ??= error; });
    for (const [index, stream] of [child.stdout, child.stderr].entries()) stream.on('data', chunk => {
      const keep = Math.min(cap - sizes[index], chunk.length);
      if (keep > 0) { captured[index].push(Buffer.from(chunk.subarray(0, keep))); sizes[index] += keep; }
      if (keep < chunk.length) {
        truncated = true;
      }
    });
    child.on('error', error => { failure ??= error; });
    child.on('exit', code => {
      exitCode = code ?? 130;
      resolveExit();
      void stop().catch(error => { failure = error; });
    });
    const finished = new Promise(resolve => child.on('close', () => { resolveExit(); resolve(); }));
    child.stdin.end();
    try {
      await finished;
      // A command may fork descendants and close their inherited pipes. Always
      // reap the owned group even when the foreground shell already exited.
      await stop();
      if (failure) throw failure;
      const decode = index => capUtf8(Buffer.concat(captured[index]).toString('utf8'), cap);
      return { stdout: decode(0), stderr: decode(1), exit_code: exitCode ?? 1, interrupted: false, truncated };
    } finally {
      clearTimeout(timer);
      signal.removeEventListener('abort', abort);
    }
  }

  async function grep(input, signal) {
    const pattern = text(input, 'pattern');
    if (!pattern || bytes(pattern) > 4096) throw new Error('pattern must be 1 to 4096 bytes');
    const mode = choice(input, 'output_mode', ['content', 'files_with_matches', 'count'], 'files_with_matches');
    const insensitive = bool(input, '-i', false);
    if ('case_sensitive' in input && '-i' in input) throw new Error('case_sensitive conflicts with -i');
    const sensitive = bool(input, 'case_sensitive', !insensitive);
    const only = bool(input, '-o', false);
    const numbered = bool(input, '-n', true);
    const multiline = bool(input, 'multiline', false);
    if ('-C' in input && 'context' in input) throw new Error('-C conflicts with context');
    const context = number(input, '-C' in input ? '-C' : 'context', 0, 0, 2000);
    const before = number(input, '-B', context, 0, 2000);
    const after = number(input, '-A', context, 0, 2000);
    const offset = number(input, 'offset', 0, 0);
    const limit = number(input, 'head_limit', 250, 0);
    if (mode !== 'content' && ['-B', '-A', '-C', 'context', '-n', '-o'].some(key => key in input)) throw new Error('context, -n, and -o require output_mode content');
    if (only && (before || after || multiline)) throw new Error('-o cannot be combined with context or multiline');
    const glob = input.glob === undefined ? undefined : text(input, 'glob');
    const matcher = glob === undefined ? undefined : globMatcher(glob);
    const search = await existing(optionalText(input, 'path', '.'));
    // Compile once, including for empty workspaces. Rust owns regex syntax,
    // Unicode case folding and line/multiline matching; Node owns file access.
    const Regex = await loadMatcher();
    const regex = new Regex(pattern, !sensitive, multiline);
    try {
      const out = boundedOutput();
      let scanned = 0;
      let skipped = 0;
      let yielded = 0;
      for (const file of await walk(search.path, signal)) {
        const name = shown(file);
        if (matcher) {
          const rel = relative(search.path, file).split(sep).join('/');
          if (!matcher(glob.includes('/') && rel ? rel : basename(file))) continue;
        }
        scanned += Math.min((await lstat(file)).size, MAX_FILE + 1);
        if (scanned > 128 * MAX_FILE) throw new Error('search exceeds 128 MiB scan limit');
        let contents;
        try { contents = await readText(file); } catch { continue; }
        const lines = rustLines(contents);
        signal.throwIfAborted();
        const result = JSON.parse(regex.scan(contents, only));
        const hits = new Set(result.hits);
        const occurrences = result.occurrences;
        if (mode !== 'content') {
          if (!hits.size) continue;
          if (skipped++ < offset) continue;
          if (limit && yielded >= limit) break;
          if (!out.push(mode === 'count' ? `${name}:${hits.size}\n` : `${name}\n`)) break;
          yielded++;
        } else if (only) {
          for (const [index, match] of occurrences) {
            if (skipped++ < offset) continue;
            if (limit && yielded >= limit) return out.value;
            if (!out.push(`${name}:${numbered ? `${index + 1}:` : ''}${match}\n`)) return out.value;
            yielded++;
          }
        } else {
          const selected = new Set();
          const emit = new Set();
          for (const index of [...hits].sort((a, b) => a - b)) {
            if (skipped++ < offset) continue;
            if (limit && yielded >= limit) break;
            selected.add(index);
            yielded++;
            for (let row = Math.max(0, index - before); row < Math.min(lines.length, index + after + 1); row++) emit.add(row);
          }
          let previous;
          for (const index of [...emit].sort((a, b) => a - b)) {
            if (previous !== undefined && index > previous + 1 && !out.push('--\n')) return out.value;
            const separator = selected.has(index) ? ':' : '-';
            if (!out.push(`${name}${separator}${numbered ? `${index + 1}${separator}` : ''}${lines[index]}\n`)) return out.value;
            previous = index;
          }
        }
        if (limit && yielded >= limit) break;
      }
      return out.value;
    } finally { regex.free(); }
  }

  async function notebook(input, signal) {
    const rel = pathOf(text(input, 'notebook_path'));
    if (!rel.endsWith('.ipynb')) throw new Error('notebook_path must end in .ipynb');
    await safeTarget(rel);
    const path = (await existing(rel, true)).path;
    const source = text(input, 'new_source');
    if (bytes(source) > 32768) throw new Error('new_source exceeds 32 KiB output-safe limit');
    const mode = choice(input, 'edit_mode', ['replace', 'insert', 'delete'], 'replace');
    const kind = input.cell_type === undefined ? undefined : choice(input, 'cell_type', ['code', 'markdown']);
    const id = input.cell_id === undefined ? undefined : text(input, 'cell_id');
    if (id === '') throw new Error('cell_id must not be empty');
    const before = await readText(path);
    const value = JSON.parse(before);
    const cells = value?.cells;
    if (!Array.isArray(cells)) throw new Error('notebook has no cells array');
    if (mode !== 'insert' && id === undefined) throw new Error('cell_id is required for replace and delete');
    let index = id === undefined ? 0 : cells.findIndex(cell => cell?.id === id);
    if (index === -1 && /^\+?\d+$/.test(id) && Number(id) < cells.length) index = Number(id);
    if (index === -1) throw new Error(`cell_id not found: ${id}`);
    const metadata = value.metadata;
    const candidateLanguage = metadata?.language_info?.name ?? metadata?.kernelspec?.language ?? metadata?.kernelspec?.name;
    const result = { new_source: source, language: typeof candidateLanguage === 'string' ? candidateLanguage : 'unknown', edit_mode: mode, notebook_path: path };
    if (mode === 'insert') {
      if (kind === undefined) throw new Error('cell_type required for insert');
      let newId;
      do { newId = `cell-${randomUUID()}`; } while (cells.some(cell => cell?.id === newId));
      const cell = { cell_type: kind, metadata: {}, source: sourceValue(source), id: newId };
      if (kind === 'code') Object.assign(cell, { execution_count: null, outputs: [] });
      cells.splice(id === undefined ? 0 : index + 1, 0, cell);
      Object.assign(result, { cell_id: newId, cell_type: kind });
    } else {
      const cell = cells[index];
      if (!isObject(cell)) throw new Error('notebook cell is not an object');
      if (typeof cell.cell_type !== 'string') throw new Error('cell has no cell_type');
      result.old_source = typeof cell.source === 'string' ? cell.source : Array.isArray(cell.source) ? cell.source.filter(item => typeof item === 'string').join('') : '';
      if (typeof cell.id === 'string') result.cell_id = cell.id;
      result.cell_type = mode === 'delete' ? cell.cell_type : kind ?? cell.cell_type;
      if (mode === 'delete') cells.splice(index, 1);
      else {
        if (!['code', 'markdown'].includes(result.cell_type)) throw new Error('cell_type must be code or markdown');
        cell.source = typeof cell.source === 'string' ? source : sourceValue(source);
        if (result.cell_type !== cell.cell_type) {
          cell.cell_type = result.cell_type;
          if (cell.cell_type === 'code') { delete cell.attachments; if (!('execution_count' in cell)) cell.execution_count = null; if (!('outputs' in cell)) cell.outputs = []; }
          else { delete cell.execution_count; delete cell.outputs; }
        }
      }
    }
    const output = jsonBounded(result, 'NotebookEdit output');
    await writeText(rel, `${JSON.stringify(value, null, 2)}\n`, signal, before);
    return output;
  }

  async function dispatch(name, input, signal) {
    switch (name) {
      case 'Read': {
        if ('pages' in input) throw new Error('Read pages is unsupported: PDF reading is not available');
        const path = (await existing(text(input, 'file_path'), true)).path;
        const contents = await readText(path);
        const offset = number(input, 'offset', 1, 1);
        const limit = Math.min(2000, number(input, 'limit', 2000, 1));
        const out = boundedOutput();
        const lines = rustLines(contents);
        for (let i = offset - 1; i < Math.min(lines.length, offset - 1 + limit); i++) if (!out.push(`${i + 1}\t${lines[i]}\n`)) break;
        return out.value;
      }
      case 'Write': {
        const rel = pathOf(text(input, 'file_path'));
        await writeText(rel, text(input, 'content'), signal);
        return `Wrote ${rel}`;
      }
      case 'Edit': {
        const rel = pathOf(text(input, 'file_path'));
        const old = text(input, 'old_string');
        const replacement = text(input, 'new_string');
        if (!old) throw new Error('old_string must not be empty');
        const all = bool(input, 'replace_all', false);
        const before = await readText((await existing(rel, true)).path);
        let count = 0;
        for (let at = before.indexOf(old); at !== -1; at = before.indexOf(old, at + old.length)) count++;
        if (!count) throw new Error('old_string not found');
        if (count !== 1 && !all) throw new Error(`old_string occurs ${count} times; set replace_all`);
        if (bytes(before) + (bytes(replacement) - bytes(old)) * (all ? count : 1) > MAX_FILE) throw new Error('edited content exceeds 1 MiB text limit');
        const updated = all ? before.split(old).join(replacement) : before.replace(old, () => replacement);
        await writeText(rel, updated, signal, before);
        return `Edited ${rel} (${count} replacement(s))`;
      }
      case 'Glob': {
        const raw = text(input, 'pattern');
        const pattern = isAbsolute(raw) ? pathOf(raw) : raw;
        const match = globMatcher(pattern);
        const search = await existing(optionalText(input, 'path', '.'));
        const prefix = pattern.split('/').slice(0, pattern.split('/').findIndex(part => /[*?]/.test(part)) < 0 ? undefined : pattern.split('/').findIndex(part => /[*?]/.test(part))).join('/');
        if (prefix) {
          try { await existing(join(search.path, prefix)); }
          catch (error) { if (error.code !== 'ENOENT' && error.code !== 'ENOTDIR') throw error; }
        }
        const out = boundedOutput();
        for (const path of await walk(search.path, signal)) if (match(relative(search.path, path).split(sep).join('/')) && !out.push(`${shown(path)}\n`)) break;
        return out.value;
      }
      case 'Grep': return grep(input, signal);
      case 'NotebookEdit': return notebook(input, signal);
      case 'Bash': {
        if ('dangerouslyDisableSandbox' in input) throw new Error('dangerouslyDisableSandbox is never supported');
        if (bool(input, 'run_in_background', false)) throw new Error('run_in_background requires an unavailable session capability');
        const command = text(input, 'command');
        if (!command.trim() || bytes(command) > 16384) throw new Error('command must be nonblank and at most 16384 bytes');
        if (input.description !== undefined && bytes(text(input, 'description')) > 1024) throw new Error('description exceeds 1024 bytes');
        const timeout = number(input, 'timeout', 120000, 1, 600000);
        if (process.platform === 'win32') throw new Error('Native Claude Bash requires a POSIX host for process-group cleanup');
        return JSON.stringify(await processRun('/bin/bash', ['--noprofile', '--norc', '-c', command], { signal, timeout }));
      }
      default: throw new Error(`unknown local Claude tool: ${name}`);
    }
  }
  return Object.freeze({
    tools: definitions.map(([name, description, inputSchema]) => Object.freeze({ name, description, inputSchema,
      async handler(input, context = {}) {
        if (closed) throw new Error('Claude tools are closed');
        const signal = context.signal ? AbortSignal.any([lifetime.signal, context.signal]) : lifetime.signal;
        signal.throwIfAborted();
        const allowed = Object.keys(inputSchema.properties);
        if (name === 'Read') allowed.push('pages');
        if (name === 'Grep') allowed.push('case_sensitive');
        fields(input, name, allowed);
        const pending = dispatch(name, input, signal);
        running.add(pending);
        try { return await pending; } finally { running.delete(pending); }
      },
    })),
    close() {
      if (!closing) {
        closed = true;
        lifetime.abort(new Error('Claude tools are closed'));
        closing = Promise.allSettled([...running]).then(() => {});
      }
      return closing;
    },
  });
}

function bytes(value) { return Buffer.byteLength(value, 'utf8'); }
function isObject(value) { return value !== null && typeof value === 'object' && !Array.isArray(value); }
function fields(value, name, allowed) {
  if (!isObject(value)) throw new TypeError(`${name} input must be an object`);
  for (const key of Object.keys(value)) if (!allowed.includes(key)) throw new Error(`unsupported ${name} option: ${key}`);
}
function text(input, key) {
  if (typeof input[key] !== 'string') throw new TypeError(`missing or invalid ${key}`);
  return input[key];
}
function optionalText(input, key, fallback) { return input[key] === undefined ? fallback : text(input, key); }
function bool(input, key, fallback) {
  if (input[key] === undefined) return fallback;
  if (typeof input[key] !== 'boolean') throw new TypeError(`invalid ${key}`);
  return input[key];
}
function number(input, key, fallback, minimum, maximum = Number.MAX_SAFE_INTEGER) {
  const value = input[key] === undefined ? fallback : input[key];
  if (!Number.isSafeInteger(value) || value < minimum || value > maximum) throw new RangeError(`invalid ${key}: expected ${minimum} to ${maximum}`);
  return value;
}
function choice(input, key, choices, fallback) {
  const value = input[key] === undefined ? fallback : text(input, key);
  if (!choices.includes(value)) throw new Error(`invalid ${key}`);
  return value;
}
function capUtf8(text, maximum) {
  const encoded = Buffer.from(text);
  if (encoded.length <= maximum) return text;
  let end = maximum;
  while (end > 0 && (encoded[end] & 0xc0) === 0x80) end--;
  return encoded.subarray(0, end).toString('utf8');
}
function boundedOutput() {
  let value = '';
  let size = 0;
  return { get value() { return value; }, push(line) {
    if (size + bytes(line) > MAX_OUTPUT) {
      if (size + 19 <= MAX_OUTPUT) value += '[output truncated]\n';
      return false;
    }
    value += line; size += bytes(line); return true;
  } };
}
function rustLines(text) {
  if (!text) return [];
  const result = text.split('\n');
  if (result.at(-1) === '') result.pop();
  return result.map((line, i) => line.endsWith('\r') && (i < result.length - 1 || text.endsWith('\n')) ? line.slice(0, -1) : line);
}
function sourceValue(source) { return source.match(/[^\n]*\n|[^\n]+$/g) ?? []; }
function jsonBounded(value, label) {
  const result = JSON.stringify(value);
  if (bytes(result) > MAX_OUTPUT) throw new Error(`${label} exceeds 64 KiB`);
  return result;
}

// Non-recursive wildcard matching avoids catastrophic JS regexp backtracking.
function globMatcher(pattern) {
  if (!pattern || bytes(pattern) > 512 || pattern.startsWith('/') || pattern.split('/').some(part => part === '.' || part === '..')) throw new Error('invalid glob pattern or traversal');
  if (/[\[\]{}\\]/.test(pattern)) throw new Error('unsupported glob syntax: only *, ? and ** are available');
  const tokens = [];
  const chars = [...pattern];
  for (let i = 0; i < chars.length; i++) {
    if (chars[i] === '*' && chars[i + 1] === '*') {
      i++;
      if (chars[i + 1] === '/') { i++; tokens.push({ kind: 'globdir' }); }
      else tokens.push({ kind: 'globstar' });
    } else if (chars[i] === '*') tokens.push({ kind: 'star' });
    else if (chars[i] === '?') tokens.push({ kind: 'any' });
    else tokens.push({ kind: 'literal', value: chars[i] });
  }
  return value => {
    // globdir is the optional .*/ prefix; its consumed branch needs a slash.
    const target = [...value];
    let reachable = new Uint8Array(target.length + 1); reachable[0] = 1;
    for (const token of tokens) {
      const next = new Uint8Array(target.length + 1);
      let active = false;
      for (let i = 0; i <= target.length; i++) {
        if (token.kind === 'globdir') {
          if (reachable[i]) { next[i] = 1; active = true; }
          if (active && target[i] === '/') next[i + 1] = 1;
        } else if (token.kind === 'star' || token.kind === 'globstar') {
          if (reachable[i]) active = true;
          if (active) next[i] = 1;
          if (token.kind === 'star' && target[i] === '/') active = false;
        } else if (reachable[i] && i < target.length && (token.kind === 'any' ? target[i] !== '/' : token.value === target[i])) next[i + 1] = 1;
      }
      reachable = next;
    }
    return reachable[target.length] === 1;
  };
}
