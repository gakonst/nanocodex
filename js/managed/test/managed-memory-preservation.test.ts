import { createHash } from 'node:crypto';
import { env, runInDurableObject } from 'cloudflare:test';
import { describe, expect, it, vi } from 'vitest';
import type { BeforeCompactionRequest, CompactionReceipt } from 'nanocodex';
import { createBrowserHost } from '../../nanocodex/browser/host.mjs';
import { ManagedSubagentBindings, managedAuthorizationForToolContext } from '../src/index';
import { preserveManagedMemory, type ManagedMemoryPreservationOptions } from '../src/managed-memory-preservation';
import type { MemoryScope } from '../src/memory-scope';

const binding = (env as unknown as { NANOCODEX_MEMORY: DurableObjectNamespace<MemoryScope> }).NANOCODEX_MEMORY;
const hash = (value: string) => createHash('sha256').update(value).digest('hex');
const receiptHash = hash('durable synthetic receipt');
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(done => { resolve = done; });
  return { promise, resolve };
}
function fixture(storage: DurableObjectStorage) {
  const controller = new AbortController();
  const sessionId = crypto.randomUUID();
  const request: BeforeCompactionRequest = {
    boundaryId: crypto.randomUUID(), sessionId, rootSessionId: sessionId,
    messages: [{ role: 'user', text: 'Use the copper fixture for this project.' }],
    truncated: false, signal: controller.signal,
  };
  const fetch = vi.fn(async (_url: RequestInfo | URL, _init?: RequestInit) =>
    Response.json({ boundary_id: request.boundaryId, request_hash: receiptHash }));
  const getByName = vi.fn((_name: string) => ({ fetch }));
  const authority = vi.fn<ManagedMemoryPreservationOptions['authority']>(() => ({ capabilities: ['memory:read', 'memory:write'] }));
  const assertActive = vi.fn();
  const options: ManagedMemoryPreservationOptions = {
    storage, organizationId: crypto.randomUUID(), teamId: 'team-fixture', ownerId: 'alice-fixture', sessionId,
    memories: { getByName } as unknown as ManagedMemoryPreservationOptions['memories'],
    enabled: true, authority, assertActive,
  };
  return { request, options, controller, fetch, getByName, authority, assertActive };
}
function withStorage(test: (storage: DurableObjectStorage) => Promise<void>) {
  return runInDurableObject(binding.getByName(crypto.randomUUID()), async (_memory, state) => test(state.storage));
}

describe('managed before-compaction preservation', () => {
  it('connects the host barrier to exact live root authority and the managed session receipt', async () => {
    await withStorage(async storage => {
      const f = fixture(storage);
      const runtimeRoot = crypto.randomUUID();
      const bindings = new ManagedSubagentBindings();
      const activeAuthorization = { capabilities: ['memory:read', 'memory:write'] as const };
      f.options.authority = request => managedAuthorizationForToolContext(bindings, runtimeRoot, activeAuthorization, request);
      const hostOptions = { toolMode: 'direct' as const,
        beforeCompaction: (request: BeforeCompactionRequest) => preserveManagedMemory(f.options, request),
      };
      const host = createBrowserHost(hostOptions) as {
        beforeCompaction(request: Omit<BeforeCompactionRequest, 'signal'>): Promise<CompactionReceipt>;
        dispose(): Promise<void>;
      };
      try {
        const wrongRoot = crypto.randomUUID();
        const skipped = { ...f.request, boundaryId: crypto.randomUUID(), sessionId: wrongRoot, rootSessionId: wrongRoot };
        expect(await host.beforeCompaction(skipped)).toEqual({ receiptId: `memory-skip:${hash(skipped.boundaryId)}` });
        expect(f.fetch).not.toHaveBeenCalled();
        expect(storage.sql.exec('SELECT reason FROM managed_memory_preservation_skips WHERE boundary_id=?', skipped.boundaryId).one())
          .toEqual({ reason: 'no_active_authority' });
        const entered = deferred<void>();
        const committed = deferred<Response>();
        f.fetch.mockImplementation(() => { entered.resolve(); return committed.promise; });
        let finished = false;
        const request = { ...f.request, sessionId: runtimeRoot, rootSessionId: runtimeRoot };
        const preserving = host.beforeCompaction(request).then(receipt => { finished = true; return receipt; });
        await entered.promise;
        expect(finished).toBe(false);
        const [, init] = f.fetch.mock.calls[0]!;
        expect(new Headers(init!.headers).get('x-nanocodex-subject-id')).toBe(`agent:${f.options.sessionId}`);
        expect(JSON.parse(init!.body as string)).toMatchObject({ session_id: f.options.sessionId, boundary_id: request.boundaryId });
        expect(f.getByName).toHaveBeenCalledExactlyOnceWith(JSON.stringify(['personal-memory', f.options.organizationId, f.options.ownerId]));
        committed.resolve(Response.json({ boundary_id: request.boundaryId, request_hash: receiptHash }));
        expect(await preserving).toEqual({ receiptId: `memory-flush:${receiptHash}` });
      } finally { await host.dispose(); }
    });
  });

  it('routes only to personal memory with matching host assertions and bounded source evidence', async () => {
    await withStorage(async storage => {
      const f = fixture(storage);
      const messages: BeforeCompactionRequest['messages'] = [
        ...f.request.messages, ...f.request.messages,
        { role: 'assistant', text: 'The copper fixture is selected.' },
      ];
      const request = { ...f.request, messages, truncated: true };
      expect(await preserveManagedMemory(f.options, request)).toEqual({ receiptId: `memory-flush:${receiptHash}` });
      expect(f.authority).toHaveBeenCalledWith(request);
      expect(f.getByName).toHaveBeenCalledExactlyOnceWith(JSON.stringify(['personal-memory', f.options.organizationId, f.options.ownerId]));
      expect(f.fetch).toHaveBeenCalledExactlyOnceWith('https://memory.internal/markdown-memory/flush', {
        method: 'POST', signal: request.signal,
        headers: {
          'content-type': 'application/json',
          'x-nanocodex-organization-id': f.options.organizationId,
          'x-nanocodex-team-id': `personal:${f.options.ownerId}`,
          'x-nanocodex-private-memory-owner': f.options.ownerId,
          'x-nanocodex-memory-initialize': '1',
          'x-nanocodex-subject-id': `agent:${f.options.sessionId}`,
          'x-nanocodex-memory-mutation': '1',
        },
        body: JSON.stringify({ boundary_id: request.boundaryId, session_id: f.options.sessionId,
          messages: [messages[0], messages[2]].map(message => ({
            id: hash(`${message.role}\0${message.text}`), role: message.role, text: message.text,
          })), truncated: true }),
      });
      expect(f.assertActive.mock.calls.length).toBeGreaterThanOrEqual(2);
    });
  });

  it('waits for the durable receipt body before releasing the compaction barrier', async () => {
    await withStorage(async storage => {
      const f = fixture(storage);
      const fetched = deferred<void>();
      let stream!: ReadableStreamDefaultController<Uint8Array>;
      f.fetch.mockImplementation(async () => {
        fetched.resolve();
        return new Response(new ReadableStream<Uint8Array>({ start(controller) { stream = controller; } }));
      });
      let settled = false;
      const preserving = preserveManagedMemory(f.options, f.request).then(receipt => { settled = true; return receipt; });
      await fetched.promise;
      await Promise.resolve();
      expect(settled).toBe(false);
      stream.enqueue(new TextEncoder().encode(JSON.stringify({ boundary_id: f.request.boundaryId, request_hash: receiptHash })));
      stream.close();
      expect(await preserving).toEqual({ receiptId: `memory-flush:${receiptHash}` });
    });
  });

  it.each(['abort', 'revoked authority'] as const)('rechecks %s after awaiting the receipt body', async failure => {
    await withStorage(async storage => {
      const f = fixture(storage);
      let active = true;
      f.assertActive.mockImplementation(() => { if (!active) throw new Error('authority revoked during receipt'); });
      const reading = deferred<void>();
      const body = deferred<{ boundary_id: string; request_hash: string }>();
      const response = Response.json({});
      vi.spyOn(response, 'json').mockImplementation(() => { reading.resolve(); return body.promise; });
      f.fetch.mockResolvedValue(response);
      const preserving = preserveManagedMemory(f.options, f.request);
      const rejected = expect(preserving).rejects.toThrow(failure === 'abort' ? 'aborted during receipt' : 'authority revoked during receipt');
      await reading.promise;
      if (failure === 'abort') f.controller.abort(new Error('aborted during receipt'));
      else active = false;
      body.resolve({ boundary_id: f.request.boundaryId, request_hash: receiptHash });
      await rejected;
    });
  });

  it.each([429, 500, 503])('blocks compaction when the preservation provider returns HTTP %s', async status => {
    await withStorage(async storage => {
      const f = fixture(storage);
      f.fetch.mockResolvedValue(new Response('provider unavailable', { status }));
      await expect(preserveManagedMemory(f.options, f.request)).rejects.toThrow(`Memory preservation failed (HTTP ${status}); context has not been compacted.`);
      expect(storage.sql.exec('SELECT * FROM managed_memory_preservation_skips').toArray()).toEqual([]);
    });
  });

  it('propagates transport failure without creating a successful skip receipt', async () => {
    await withStorage(async storage => {
      const f = fixture(storage);
      f.fetch.mockRejectedValue(new Error('synthetic transport failure'));
      await expect(preserveManagedMemory(f.options, f.request)).rejects.toThrow('synthetic transport failure');
      expect(storage.sql.exec('SELECT * FROM managed_memory_preservation_skips').toArray()).toEqual([]);
    });
  });

  it.each([
    { boundary_id: 'different-boundary', request_hash: receiptHash },
    { request_hash: receiptHash },
    { request_hash: 'not-a-durable-hash', matchingBoundary: true },
    { matchingBoundary: true },
  ])('rejects an invalid preservation receipt: %j', async ({ matchingBoundary, ...body }) => {
    await withStorage(async storage => {
      const f = fixture(storage);
      f.fetch.mockResolvedValue(Response.json({ ...body, ...(matchingBoundary ? { boundary_id: f.request.boundaryId } : {}) }));
      await expect(preserveManagedMemory(f.options, f.request)).rejects.toThrow('invalid durable receipt');
    });
  });

  it('aborts before touching durable state or dispatching a flush', async () => {
    await withStorage(async storage => {
      const f = fixture(storage);
      f.controller.abort(new Error('aborted before preservation'));
      await expect(preserveManagedMemory(f.options, f.request)).rejects.toThrow('aborted before preservation');
      expect(f.fetch).not.toHaveBeenCalled();
      expect(f.assertActive).not.toHaveBeenCalled();
      expect(storage.sql.exec("SELECT name FROM sqlite_master WHERE name='managed_memory_preservation_skips'").toArray()).toEqual([]);
    });
  });

  it('does not release the barrier after an abort during the flush', async () => {
    await withStorage(async storage => {
      const f = fixture(storage);
      const response = deferred<Response>();
      f.fetch.mockImplementation(() => response.promise);
      const preserving = preserveManagedMemory(f.options, f.request);
      const rejected = expect(preserving).rejects.toThrow('aborted during preservation');
      expect(f.fetch).toHaveBeenCalledTimes(1);
      f.controller.abort(new Error('aborted during preservation'));
      response.resolve(Response.json({ boundary_id: f.request.boundaryId, request_hash: receiptHash }));
      await rejected;
    });
  });

  it.each(['before', 'during'] as const)('blocks a stale runtime when active authority is revoked %s the flush', async when => {
    await withStorage(async storage => {
      const f = fixture(storage);
      let active = when !== 'before';
      f.assertActive.mockImplementation(() => { if (!active) throw new Error('authority revoked'); });
      const response = deferred<Response>();
      f.fetch.mockImplementation(() => response.promise);
      const preserving = preserveManagedMemory(f.options, f.request);
      const rejected = expect(preserving).rejects.toThrow('authority revoked');
      active = false;
      response.resolve(Response.json({ boundary_id: f.request.boundaryId, request_hash: receiptHash }));
      await rejected;
      expect(f.fetch).toHaveBeenCalledTimes(when === 'before' ? 0 : 1);
    });
  });

  it.each([
    ['disabled', 'disabled'],
    ['connect', 'connect_team_sharing_requires_explicit_write'],
    ['subagent', 'subagent'],
    ['missing authority', 'no_active_authority'],
    ['read only', 'memory_capability_disabled'],
    ['write only', 'memory_capability_disabled'],
    ['no capabilities', 'memory_capability_disabled'],
  ])('durably skips %s boundaries without expanding authority on replay', async (mode, reason) => {
    await withStorage(async storage => {
      const f = fixture(storage);
      let request = f.request;
      if (mode === 'disabled') f.options.enabled = false;
      if (mode === 'connect') f.authority.mockReturnValue({ capabilities: ['memory:read', 'memory:write'], connectGrant: { id: 'synthetic-grant' } });
      if (mode === 'subagent') request = { ...request, sessionId: crypto.randomUUID() };
      if (mode === 'missing authority') f.authority.mockReturnValue(undefined);
      if (mode === 'read only') f.authority.mockReturnValue({ capabilities: ['memory:read'] });
      if (mode === 'write only') f.authority.mockReturnValue({ capabilities: ['memory:write'] });
      if (mode === 'no capabilities') f.authority.mockReturnValue({ capabilities: [] });
      const expected = { receiptId: `memory-skip:${hash(request.boundaryId)}` };
      expect(await preserveManagedMemory(f.options, request)).toEqual(expected);
      expect(storage.sql.exec('SELECT boundary_id,reason FROM managed_memory_preservation_skips').toArray()).toEqual([
        { boundary_id: request.boundaryId, reason },
      ]);
      f.authority.mockReturnValue({ capabilities: ['memory:read', 'memory:write'] });
      // Reconstruct the callback with enabled settings; only the durable ledger survives.
      expect(await preserveManagedMemory({ ...f.options, enabled: true }, { ...request })).toEqual(expected);
      expect(f.getByName).not.toHaveBeenCalled();
      expect(storage.sql.exec('SELECT COUNT(*) AS count FROM managed_memory_preservation_skips').one()).toEqual({ count: 1 });
      const next = { ...f.request, boundaryId: crypto.randomUUID() };
      f.fetch.mockResolvedValue(Response.json({ boundary_id: next.boundaryId, request_hash: receiptHash }));
      expect(await preserveManagedMemory({ ...f.options, enabled: true }, next)).toEqual({ receiptId: `memory-flush:${receiptHash}` });
      expect(f.fetch).toHaveBeenCalledTimes(1);
    });
  });

  it.each(['messages', 'sessionId', 'rootSessionId', 'truncated'] as const)('rejects skipped-boundary replay with changed %s', async field => {
    await withStorage(async storage => {
      const f = fixture(storage);
      await preserveManagedMemory({ ...f.options, enabled: false }, f.request);
      const altered = { ...f.request,
        ...(field === 'messages' ? { messages: [{ role: 'user' as const, text: 'Different evidence' }] } : {}),
        ...(field === 'sessionId' ? { sessionId: crypto.randomUUID() } : {}),
        ...(field === 'rootSessionId' ? { rootSessionId: crypto.randomUUID() } : {}),
        ...(field === 'truncated' ? { truncated: true } : {}),
      };
      await expect(preserveManagedMemory(f.options, altered)).rejects.toThrow('boundary reused with different evidence');
      expect(f.fetch).not.toHaveBeenCalled();
      expect(await preserveManagedMemory(f.options, f.request)).toEqual({ receiptId: `memory-skip:${hash(f.request.boundaryId)}` });
    });
  });

  it('fails closed against the real personal MemoryScope when AI is unconfigured', async () => {
    await withStorage(async storage => {
      const f = fixture(storage);
      await expect(preserveManagedMemory({ ...f.options, memories: binding }, f.request))
        .rejects.toThrow('Memory preservation failed (HTTP 503); context has not been compacted.');
      expect(storage.sql.exec('SELECT * FROM managed_memory_preservation_skips').toArray()).toEqual([]);
    });
  });
});
