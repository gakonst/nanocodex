import { createHash } from 'node:crypto';
import type { BeforeCompactionRequest, CompactionReceipt } from 'nanocodex';
import { memoryTarget } from './memory-target';

export interface ManagedMemoryPreservationOptions {
  storage: DurableObjectStorage;
  memories: DurableObjectNamespace<import('./memory-scope').MemoryScope>;
  organizationId: string;
  teamId: string;
  ownerId: string;
  sessionId: string;
  enabled: boolean;
  /** Resolve active authority for this exact root runtime at each barrier. */
  authority(request: BeforeCompactionRequest): { capabilities: readonly string[]; connectGrant?: unknown } | undefined;
  assertActive(): void;
}
const hash = (value: string) => createHash('sha256').update(value).digest('hex');

/** The runtime awaits this receipt before discarding context. No model-visible tool
 * can invoke this internal RPC or choose an owner/organization for its evidence. */
export async function preserveManagedMemory(options: ManagedMemoryPreservationOptions,
  request: BeforeCompactionRequest): Promise<CompactionReceipt> {
  request.signal.throwIfAborted();
  options.assertActive();
  options.storage.sql.exec(`CREATE TABLE IF NOT EXISTS managed_memory_preservation_skips (
    boundary_id TEXT PRIMARY KEY, request_hash TEXT NOT NULL, reason TEXT NOT NULL)`);
  const requestHash = hash(JSON.stringify({ session: request.sessionId, root: request.rootSessionId,
    messages: request.messages, truncated: request.truncated }));
  const skipped = options.storage.sql.exec<{request_hash: string}>(
    'SELECT request_hash FROM managed_memory_preservation_skips WHERE boundary_id=?', request.boundaryId).toArray()[0];
  if (skipped) {
    if (skipped.request_hash !== requestHash) throw new Error('Memory preservation boundary reused with different evidence');
    return { receiptId: `memory-skip:${hash(request.boundaryId)}` };
  }
  const authority = options.authority(request);
  const disabled = !options.enabled ? 'disabled'
    : request.sessionId !== request.rootSessionId ? 'subagent'
    : !authority ? 'no_active_authority'
    : authority.connectGrant ? 'connect_team_sharing_requires_explicit_write'
    : !authority.capabilities.includes('memory:read') || !authority.capabilities.includes('memory:write') ? 'memory_capability_disabled' : undefined;
  if (disabled) {
    // A durable local no-op keeps replay from expanding authority after settings change.
    options.storage.sql.exec('INSERT OR IGNORE INTO managed_memory_preservation_skips VALUES(?,?,?)', request.boundaryId, requestHash, disabled);
    return { receiptId: `memory-skip:${hash(request.boundaryId)}` };
  }
  const target = memoryTarget(options.organizationId, options.teamId, options.ownerId, 'personal');
  const messages = [...new Map(request.messages.map(message => {
    const id = hash(`${message.role}\0${message.text}`);
    return [id, { id, role: message.role, text: message.text }] as const;
  })).values()];
  const response = await options.memories.getByName(target.name).fetch('https://memory.internal/markdown-memory/flush', {
    method: 'POST', signal: request.signal,
    headers: {
      'content-type': 'application/json',
      'x-nanocodex-organization-id': options.organizationId,
      'x-nanocodex-team-id': target.team,
      'x-nanocodex-private-memory-owner': options.ownerId,
      'x-nanocodex-memory-initialize': '1',
      'x-nanocodex-subject-id': `agent:${options.sessionId}`,
      'x-nanocodex-memory-mutation': '1',
    },
    body: JSON.stringify({ boundary_id: request.boundaryId, session_id: options.sessionId, messages, truncated: request.truncated }),
  });
  request.signal.throwIfAborted();
  options.assertActive();
  if (!response.ok) {
    await response.body?.cancel();
    throw new Error(`Memory preservation failed (HTTP ${response.status}); context has not been compacted.`);
  }
  const receipt = await response.json<{ boundary_id?: string; request_hash?: string }>();
  request.signal.throwIfAborted();
  options.assertActive();
  if (receipt.boundary_id !== request.boundaryId || typeof receipt.request_hash !== 'string' || !/^[a-f0-9]{64}$/.test(receipt.request_hash))
    throw new Error('Memory preservation returned an invalid durable receipt');
  return { receiptId: `memory-flush:${receipt.request_hash}` };
}
