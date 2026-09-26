import { MarkdownMemoryError, MarkdownMemoryStore, validateMarkdownMemoryOwner } from './markdown-memory';
import type { MarkdownMemoryFlush, MarkdownMemoryFlushInput, MarkdownMemoryFlushReceipt } from './markdown-memory-flush';

const MAX_ACTIVE = 8;
const safeId = (value: unknown): value is string => typeof value === 'string' && /^[A-Za-z0-9._:-]{1,160}$/.test(value);

/** Immediate, optional processing of individual message events. There is no
 * capture backlog, timer, retry loop, or dependency on conversation durability.
 * Existing flush validation/receipts own exact evidence and duplicate delivery.
 */
export class MarkdownMemoryCapture {
  private readonly active = new Map<string, { owner: string; boundary: string; controller: AbortController }>();

  constructor(private readonly storage: DurableObjectStorage, private readonly flush: MarkdownMemoryFlush,
    private readonly onRetract?: (owner: string, path: string, revision: number) => void) {
    // A withdrawal may arrive before its detached message delivery. Retain only
    // its identity so a late event cannot recreate withdrawn evidence.
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS markdown_memory_capture_withdrawals (
      owner TEXT NOT NULL, boundary_id TEXT NOT NULL, PRIMARY KEY(owner,boundary_id));`);
  }

  capture(owner: string, input: MarkdownMemoryFlushInput): Promise<MarkdownMemoryFlushReceipt | undefined> {
    this.identity(owner, input?.boundary_id);
    if (!Array.isArray(input.messages) || input.messages.some(message => message?.role !== 'user'))
      throw new MarkdownMemoryError('capture requires firsthand user messages', 400, 'invalid_memory_capture');
    if (this.storage.sql.exec('SELECT 1 FROM markdown_memory_capture_withdrawals WHERE owner=? AND boundary_id=?',
      owner, input.boundary_id).toArray().length) return Promise.resolve(undefined);
    const key = JSON.stringify([owner, input.boundary_id]);
    const prior = this.active.get(key);
    if (prior) return this.flush.flush(owner, input, prior.controller.signal);
    if (this.active.size >= MAX_ACTIVE)
      throw new MarkdownMemoryError('memory capture is busy', 429, 'memory_capture_busy');
    const controller = new AbortController();
    this.active.set(key, { owner, boundary: input.boundary_id, controller });
    return this.flush.flush(owner, input, controller.signal).finally(() => { this.active.delete(key); });
  }

  cancel(owner: string, boundary: string): void {
    this.identity(owner, boundary);
    this.storage.transactionSync(() => {
      this.storage.sql.exec('INSERT OR IGNORE INTO markdown_memory_capture_withdrawals VALUES(?,?)', owner, boundary);
      const row = this.storage.sql.exec<{ receipt: string | null }>(
        'SELECT receipt FROM markdown_memory_flushes WHERE owner=? AND boundary_id=?', owner, boundary).toArray()[0];
      const receipt = row?.receipt ? JSON.parse(row.receipt) as MarkdownMemoryFlushReceipt : undefined;
      if (receipt?.path && receipt.revision !== null) {
        // Retract this generated note, but preserve subsequent explicit curation.
        const result = new MarkdownMemoryStore(this.storage).write(owner, {
          operation: 'delete', path: receipt.path, expected_revision: receipt.revision,
        });
        if (result.ok) this.onRetract?.(owner, result.path, result.revision);
      }
    });
    this.active.get(JSON.stringify([owner, boundary]))?.controller.abort();
  }

  /** Manual curation/deletion wins over any currently running capture. */
  cancelActive(owner: string): void {
    for (const source of this.active.values()) if (source.owner === owner) this.cancel(owner, source.boundary);
  }

  status(owner: string) {
    return { mode: 'on_message' as const, active: [...this.active.values()].filter(source => source.owner === owner).length,
      ...this.flush.status(owner) };
  }

  private identity(owner: string, boundary: unknown): void {
    owner = validateMarkdownMemoryOwner(owner);
    if (!owner.startsWith('personal:') || owner.length === 'personal:'.length || !safeId(boundary))
      throw new MarkdownMemoryError('invalid private capture identity', 400, 'invalid_memory_capture');
  }
}
