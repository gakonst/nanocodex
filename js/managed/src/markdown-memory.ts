import { createHash } from "node:crypto";
/** Canonical Markdown bodies and their derived search index share one DO transaction. */
const MAX_BYTES = 65_536;
const encoder = new TextEncoder();
const MAX_LINE_BYTES = 8192;
const MAX_READ_BYTES = 16384;
const MAX_BOOTSTRAP_BYTES = 12288;
function prefixBytes(value: string, max: number): string {
  let result = '', bytes = 0;
  for (const char of value) {
    const size = encoder.encode(char).length;
    if (bytes + size > max) break;
    result += char; bytes += size;
  }
  return result;
}
export class MarkdownMemoryError extends Error {
  constructor(message: string, readonly status = 400, readonly code = 'invalid_memory_input') {
    super(message);
    this.name = 'MarkdownMemoryError';
  }
}
type Row = { revision: number; deleted: number; content: string };
function record(input: unknown, keys: string[]): Record<string, unknown> {
  if (!input || typeof input !== 'object' || Array.isArray(input)) throw new MarkdownMemoryError('memory input must be an object');
  const value = input as Record<string, unknown>;
  if (Object.keys(value).some(key => !keys.includes(key))) throw new MarkdownMemoryError('unknown memory input field');
  return value;
}
function integer(value: unknown, fallback: number, min: number, max: number): number {
  if (value === undefined) return fallback;
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < min || value > max) throw new MarkdownMemoryError('invalid memory integer');
  return value;
}
function text(value: unknown, max: number): string {
  if (typeof value !== 'string' || encoder.encode(value).length > max || value.includes('\0')) throw new MarkdownMemoryError('invalid memory text');
  return value;
}
export function validateMarkdownMemoryPath(value: unknown): string {
  const path = text(value, 128);
  if (path === 'MEMORY.md' || path === 'USER.md') return path;
  const match = /^memory\/(\d{4}-\d{2}-\d{2})(?:-[a-z0-9]+(?:-[a-z0-9]+)*)?\.md$/.exec(path);
  if (!match || !Number.isFinite(Date.parse(`${match[1]}T00:00:00Z`)) || new Date(`${match[1]}T00:00:00Z`).toISOString().slice(0, 10) !== match[1]) throw new MarkdownMemoryError('invalid memory path');
  return path;
}
function ownerKey(owner: unknown): string {
  const value = text(owner, 512);
  if (!value.trim()) throw new MarkdownMemoryError('memory owner is required');
  return value;
}
export class MarkdownMemoryStore {
  constructor(private readonly storage: DurableObjectStorage) {
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS markdown_memory_documents (
      owner TEXT NOT NULL, path TEXT NOT NULL, revision INTEGER NOT NULL, deleted INTEGER NOT NULL,
      content TEXT NOT NULL, PRIMARY KEY(owner,path));
      CREATE TABLE IF NOT EXISTS markdown_memory_chunks (
        id INTEGER PRIMARY KEY AUTOINCREMENT, owner TEXT NOT NULL, path TEXT NOT NULL);
      CREATE INDEX IF NOT EXISTS markdown_memory_chunks_owner_path ON markdown_memory_chunks(owner,path);
      CREATE VIRTUAL TABLE IF NOT EXISTS markdown_memory_fts USING fts5(
        owner UNINDEXED, path UNINDEXED, revision UNINDEXED, from_line UNINDEXED, to_line UNINDEXED, content);
      CREATE TABLE IF NOT EXISTS markdown_memory_operations (
        owner TEXT NOT NULL, operation_id TEXT NOT NULL, request TEXT NOT NULL, result TEXT NOT NULL,
        PRIMARY KEY(owner,operation_id));`);
  }
  private row(owner: string, path: string): Row {
    return this.storage.sql.exec<Row>('SELECT revision,deleted,content FROM markdown_memory_documents WHERE owner=? AND path=?', owner, path).toArray()[0]
      ?? { revision: 0, deleted: 1, content: '' };
  }
  /** Compatibility projection: canonical live paths only, never tombstones. */
  list(owner: string): string[] {
    owner = ownerKey(owner);
    return this.storage.sql.exec<{ path: string }>(
      'SELECT path FROM markdown_memory_documents WHERE owner=? AND deleted=0 ORDER BY path', owner,
    ).toArray().map(row => row.path);
  }
  /** Full canonical body remains bounded by the document admission limit. */
  readFile(owner: string, path: string): string {
    owner = ownerKey(owner);
    path = validateMarkdownMemoryPath(path);
    const row = this.row(owner, path);
    if (row.deleted) throw new MarkdownMemoryError('memory file was not found', 404, 'memory_not_found');
    return row.content;
  }
  get(owner: string, input: unknown) {
    owner = ownerKey(owner);
    const value = record(input, ['path', 'from_line', 'max_lines', 'revision']);
    const path = validateMarkdownMemoryPath(value.path);
    const from = integer(value.from_line, 1, 1, MAX_BYTES + 1);
    const count = integer(value.max_lines, 100, 1, 200);
    const row = this.row(owner, path);
    if (value.revision !== undefined && integer(value.revision, 0, 0, Number.MAX_SAFE_INTEGER) !== row.revision) throw new MarkdownMemoryError('memory revision changed; restart reading', 409, 'revision_conflict');
    const lines = row.deleted || !row.content ? [] : row.content.split('\n');
    const selected: string[] = [];
    let bytes = 0;
    for (const line of lines.slice(from - 1, from - 1 + count)) {
      const size = encoder.encode(line).length + (selected.length ? 1 : 0);
      if (bytes + size > MAX_READ_BYTES) break;
      selected.push(line); bytes += size;
    }
    const to = selected.length ? from + selected.length - 1 : from - 1;
    return { path, revision: row.revision, deleted: Boolean(row.deleted), content: selected.join('\n'), from_line: from,
      to_line: to, total_lines: lines.length, ...(to < lines.length ? { next_line: to + 1 } : {}) };
  }
  search(owner: string, input: unknown) {
    owner = ownerKey(owner);
    const value = record(input, ['query', 'limit']);
    const query = text(value.query, 512);
    const limit = integer(value.limit, 8, 1, 20);
    // Quote lexical terms: caller input never becomes FTS syntax or SQL.
    const terms = query.match(/[\p{L}\p{N}_]+/gu)?.slice(0, 24) ?? [];
    if (!terms.length) return { results: [] };
    const match = terms.map(term => `"${term}"`).join(' AND ');
    const results = this.storage.sql.exec<{ path: string; revision: number; from_line: number; to_line: number; snippet: string }>(
      `SELECT path, CAST(revision AS INTEGER) AS revision, CAST(from_line AS INTEGER) AS from_line,
        CAST(to_line AS INTEGER) AS to_line, snippet(markdown_memory_fts,5,'','',' … ',48) AS snippet
        FROM markdown_memory_fts WHERE markdown_memory_fts MATCH ? AND owner=? ORDER BY rank LIMIT ?`, match, owner, limit,
    ).toArray().map(row => ({ ...row, snippet: row.snippet.slice(0, 2048) }));
    return { results };
  }
  write(owner: string, input: unknown) {
    owner = ownerKey(owner);
    const value = record(input, ['operation', 'path', 'expected_revision', 'content', 'operation_id']);
    const path = validateMarkdownMemoryPath(value.path);
    const operation = value.operation;
    if (operation !== 'put' && operation !== 'delete' && operation !== 'append') throw new MarkdownMemoryError('invalid memory operation');
    if (value.expected_revision === undefined) throw new MarkdownMemoryError('expected_revision is required (0 for new documents)');
    const expected = integer(value.expected_revision, 0, 0, Number.MAX_SAFE_INTEGER - 1);
    const content = operation === 'delete' ? '' : text(value.content, MAX_BYTES);
    if (operation === 'delete' && value.content !== undefined) throw new MarkdownMemoryError('delete does not accept content');
    let operationId: string | undefined;
    if (operation === 'append') {
      if (!path.startsWith('memory/')) throw new MarkdownMemoryError('append requires a daily memory path');
      operationId = text(value.operation_id, 128);
      if (!/^[A-Za-z0-9_-]{1,128}$/.test(operationId) || !content) throw new MarkdownMemoryError('append requires operation_id and nonempty content');
    } else if (value.operation_id !== undefined) throw new MarkdownMemoryError('operation_id is only supported for append');
    const request = createHash("sha256").update(JSON.stringify([operation, path, expected, content])).digest("hex");
    return this.storage.transactionSync(() => {
      if (operationId) {
        const prior = this.storage.sql.exec<{ request: string; result: string }>(
          'SELECT request,result FROM markdown_memory_operations WHERE owner=? AND operation_id=?', owner, operationId).toArray()[0];
        if (prior) {
          if (prior.request !== request) throw new MarkdownMemoryError('operation_id was already used with different input', 409, 'operation_id_conflict');
          return { ...JSON.parse(prior.result) as { ok: true; path: string; revision: number; deleted: boolean }, replayed: true };
        }
      }
      const row = this.row(owner, path);
      if (row.revision !== expected) return { ok: false as const, error: 'revision_conflict' as const, path, revision: row.revision };
      const body = operation === 'append' && !row.deleted && row.content
        ? row.content + (row.content.endsWith('\n') ? '' : '\n') + content : content;
      text(body, MAX_BYTES);
      if (body.split('\n').some(line => encoder.encode(line).length > MAX_LINE_BYTES)) {
        throw new MarkdownMemoryError('memory lines must not exceed 8192 UTF8 bytes');
      }
      const revision = row.revision + 1;
      const deleted = operation === 'delete';
      // Keep tombstones so a stale create cannot resurrect deleted content.
      this.storage.sql.exec(`INSERT INTO markdown_memory_documents VALUES(?,?,?,?,?)
        ON CONFLICT(owner,path) DO UPDATE SET revision=excluded.revision,deleted=excluded.deleted,content=excluded.content`,
      owner, path, revision, Number(deleted), body);
      this.storage.sql.exec(`DELETE FROM markdown_memory_fts WHERE rowid IN
        (SELECT id FROM markdown_memory_chunks WHERE owner=? AND path=?)`, owner, path);
      this.storage.sql.exec('DELETE FROM markdown_memory_chunks WHERE owner=? AND path=?', owner, path);
      if (!deleted) this.index(owner, path, revision, body);
      const result = { ok: true as const, path, revision, deleted };
      if (operationId) this.storage.sql.exec('INSERT INTO markdown_memory_operations VALUES(?,?,?,?)', owner, operationId, request, JSON.stringify(result));
      return result;
    });
  }
  private index(owner: string, path: string, revision: number, content: string) {
    const lines = content.split('\n');
    let chunk = '', from = 1, to = 1;
    const flush = () => {
      if (chunk) {
        const { id } = this.storage.sql.exec<{ id: number }>(
          'INSERT INTO markdown_memory_chunks(owner,path) VALUES(?,?) RETURNING id', owner, path,
        ).one();
        this.storage.sql.exec('INSERT INTO markdown_memory_fts(rowid,owner,path,revision,from_line,to_line,content) VALUES(?,?,?,?,?,?,?)', id, owner, path, revision, from, to, chunk);
      }
      chunk = '';
    };
    for (let i = 0; i < lines.length; i++) {
      const line = lines[i]!;
      if (chunk && (chunk.length + line.length > 2048 || i + 1 - from >= 24)) flush();
      if (line.length > 2048) {
        for (let offset = 0; offset < line.length; offset += 1536) {
          from = to = i + 1; chunk = line.slice(offset, offset + 2048); flush();
        }
      } else {
        if (!chunk) from = i + 1;
        chunk += (chunk ? '\n' : '') + line; to = i + 1;
      }
    }
    flush();
  }
  bootstrap(owner: string, now: number) {
    owner = ownerKey(owner);
    if (!Number.isFinite(now) || !Number.isFinite(new Date(now).getTime())) throw new MarkdownMemoryError('invalid bootstrap time');
    const dates = [now, now - 86_400_000].map(time => new Date(time).toISOString().slice(0, 10));
    const paths = ['MEMORY.md', 'USER.md', ...dates.map(date => `memory/${date}.md`)];
    // Canonical daily pages first, followed by bounded dated topic notes.
    const daily = this.storage.sql.exec<{ path: string }>(`SELECT path FROM markdown_memory_documents
      WHERE owner=? AND deleted=0 AND (path GLOB ? OR path GLOB ?) ORDER BY path DESC LIMIT 4`,
    owner, `memory/${dates[0]}-*.md`, `memory/${dates[1]}-*.md`).toArray();
    let remaining = MAX_BOOTSTRAP_BYTES;
    const documents = [];
    for (const path of [...paths, ...daily.map(row => row.path)]) {
      if (!remaining) break;
      const doc = this.get(owner, { path, max_lines: 40 });
      if (doc.deleted) continue;
      const content = prefixBytes(doc.content, Math.min(4096, remaining));
      remaining -= encoder.encode(content).length;
      documents.push({ path: doc.path, revision: doc.revision, content, truncated: content.length < doc.content.length || doc.next_line !== undefined });
    }
    return { documents };
  }
}
