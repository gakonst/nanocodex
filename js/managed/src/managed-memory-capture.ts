import { createHash } from 'node:crypto';
import type { PromptInput } from 'nanocodex';
import { durablePlacementOptions } from 'nanocodex/cloudflare/durable-placement';
import { memoryTarget } from './memory-target';
import type { MemoryScope } from './memory-scope';
import { unsafeMemoryEvidence, type MarkdownMemoryFlushInput } from './markdown-memory-flush';

const hash = (value: unknown) => createHash('sha256').update(JSON.stringify(value)).digest('hex');
const encoder = new TextEncoder();
export const memoryCaptureBoundary = (sessionId: string, sourceKey: string) => `capture-${hash([sessionId, sourceKey])}`;
const unescapeXml = (value: string) => value.replaceAll('&lt;', '<').replaceAll('&gt;', '>')
  .replaceAll('&quot;', '"').replaceAll('&apos;', "'").replaceAll('&amp;', '&');

/** Capture only the protocol's structured firsthand transcript. Legacy flattened
 * display text and provider handoff instructions have ambiguous attribution. */
export function voiceMemoryMessages(input: string): string[] {
  if (!input.startsWith('<realtime_delegation>') || !input.trimEnd().endsWith('</realtime_delegation>')) return [];
  const fields = [...input.matchAll(/<transcript_json>([\s\S]*?)<\/transcript_json>/g)];
  if (fields.length !== 1 || encoder.encode(fields[0]![1]!).length > 4096) return [];
  try {
    const value = JSON.parse(unescapeXml(fields[0]![1]!));
    if (!value || typeof value !== 'object' || Array.isArray(value) || value.truncated !== false
      || Object.keys(value).some(key => !['truncated', 'entries'].includes(key))
      || !Array.isArray(value.entries) || value.entries.length > 128) return [];
    const texts: string[] = [];
    for (const entry of value.entries) {
      if (!entry || typeof entry !== 'object' || Array.isArray(entry)
        || Object.keys(entry).some(key => !['role', 'text'].includes(key))
        || !['user', 'assistant'].includes(entry.role) || typeof entry.text !== 'string') return [];
      if (entry.role === 'user') texts.push(entry.text);
    }
    return texts;
  } catch { return []; }
}

/** Bound the whole source; never clip a sentence, negation, or later correction. */
export function memoryCaptureInput(sessionId: string, sourceKey: string, input: PromptInput,
  createdAt: number, voice = false): MarkdownMemoryFlushInput | undefined {
  const text = typeof input === 'string' ? input : input.flatMap(item => item.type === 'text' ? [item.text] : []).join('\n');
  if (!text || text.length > 65_536) return undefined;
  const texts = voice ? voiceMemoryMessages(text) : [text];
  if (!texts.length || texts.length > 128 || texts.some(text => unsafeMemoryEvidence(text))) return undefined;
  const result: MarkdownMemoryFlushInput = {
    boundary_id: memoryCaptureBoundary(sessionId, sourceKey), session_id: sessionId,
    messages: texts.map((text, index) => ({ id: hash([sourceKey, index]), role: 'user' as const, text, created_at: createdAt })),
  };
  return encoder.encode(JSON.stringify(result)).length <= 65_536 ? result : undefined;
}

export interface ManagedMemoryCaptureOptions {
  organizationId: string; teamId: string; ownerId: string; sessionId: string;
  capabilities: readonly string[]; connectGrant?: unknown;
  memories: DurableObjectNamespace<MemoryScope>; clientIngressColo?: string | null;
  waitUntil: (task: Promise<void>) => void;
  /** Ownership/lifecycle checks apply to both new capture and withdrawals. */
  active: () => boolean;
  /** Settings gate new capture, but must never prevent withdrawal cleanup. */
  allowed: () => boolean;
}

/** No caller awaits memory. Failures and timeouts cannot change admission, execution or recovery. */
export function scheduleMemoryCapture(options: ManagedMemoryCaptureOptions, sourceKey: string,
  source: { input: PromptInput; createdAt: number; voice?: boolean } | { cancel: true }): void {
  try {
    const task = Promise.resolve().then(async () => {
      if (options.connectGrant !== undefined || !options.active()) return;
      if (!('cancel' in source) && (!options.capabilities.includes('memory:read')
        || !options.capabilities.includes('memory:write') || !options.allowed())) return;
      const body = 'cancel' in source
        ? { session_id: options.sessionId, boundary_id: memoryCaptureBoundary(options.sessionId, sourceKey), cancel: true }
        : memoryCaptureInput(options.sessionId, sourceKey, source.input, source.createdAt, source.voice);
      if (!body) return;
      const target = memoryTarget(options.organizationId, options.teamId, options.ownerId, 'personal');
      const stub = options.memories.getByName(target.name, durablePlacementOptions(options.clientIngressColo));
      const response = await stub.fetch('https://memory.internal/markdown-memory/capture', {
        method: 'POST', signal: AbortSignal.timeout(5_000),
        headers: { 'content-type': 'application/json', 'x-nanocodex-organization-id': options.organizationId,
          'x-nanocodex-team-id': target.team, 'x-nanocodex-private-memory-owner': options.ownerId,
          'x-nanocodex-memory-initialize': '1', 'x-nanocodex-subject-id': `agent:${options.sessionId}`,
          'x-nanocodex-memory-mutation': '1' }, body: JSON.stringify(body),
      });
      await response.body?.cancel();
      if (!response.ok) console.warn({ type: 'memory_capture.delivery_failed', status: response.status });
    }).catch(() => { console.warn({ type: 'memory_capture.delivery_failed' }); });
    options.waitUntil(task);
  } catch {
    // Even a disposed execution context must not make memory part of live success.
  }
}
