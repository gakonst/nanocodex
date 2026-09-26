import { describe, expect, it, vi } from 'vitest';
import { memoryCaptureInput, scheduleMemoryCapture, voiceMemoryMessages, type ManagedMemoryCaptureOptions } from '../src/managed-memory-capture';

const session = '018f0000-0000-7000-8000-000000000001';
const escape = (text: string) => text.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;');
const voice = (entries: { role: string; text: string }[], truncated = false) =>
  `<realtime_delegation><input>Provider handoff</input><transcript_delta>Display text only</transcript_delta><transcript_json>${escape(JSON.stringify({ entries, truncated }))}</transcript_json></realtime_delegation>`;
function fixture(overrides: Partial<ManagedMemoryCaptureOptions> = {}) {
  const tasks: Promise<void>[] = [];
  const fetch = vi.fn(async (_url: string, _init?: RequestInit) => new Response(null, { status: 202 }));
  const getByName = vi.fn((_name: string) => ({ fetch }));
  const options: ManagedMemoryCaptureOptions = {
    organizationId: 'fixture-org', teamId: 'fixture-team', ownerId: 'fixture-owner', sessionId: session,
    capabilities: ['memory:read', 'memory:write'], memories: { getByName } as unknown as ManagedMemoryCaptureOptions['memories'],
    waitUntil: task => { tasks.push(task); }, active: () => true, allowed: () => true, ...overrides,
  };
  return { options, tasks, fetch, getByName };
}

describe('background memory delivery', () => {
  it('defers all work and returns while the recipient is unavailable', async () => {
    const f = fixture();
    let release!: () => void;
    f.fetch.mockImplementation(() => new Promise(resolve => { release = () => resolve(new Response(null, { status: 503 })); }));
    expect(scheduleMemoryCapture(f.options, 'turn:one', { input: 'I prefer concise replies.', createdAt: 10 })).toBeUndefined();
    expect(f.getByName).not.toHaveBeenCalled();
    await Promise.resolve();
    expect(f.fetch).toHaveBeenCalledOnce();
    release();
    await expect(Promise.all(f.tasks)).resolves.toEqual([undefined]);
  });
  it('contains recipient and execution-context failures', async () => {
    const f = fixture();
    f.getByName.mockImplementation(() => { throw new Error('private-provider-error'); });
    scheduleMemoryCapture(f.options, 'turn:one', { input: 'I prefer concise replies.', createdAt: 10 });
    await expect(Promise.all(f.tasks)).resolves.toEqual([undefined]);
    expect(() => scheduleMemoryCapture({ ...f.options, waitUntil: () => { throw new Error('disposed'); } },
      'turn:two', { input: 'I prefer concise replies.', createdAt: 11 })).not.toThrow();
  });
  it.each([
    { connectGrant: {} }, { capabilities: ['memory:read'] }, { capabilities: ['memory:write'] }, { allowed: () => false },
  ])('does not capture without direct private authority and enabled memory: %j', async override => {
    const f = fixture(override);
    scheduleMemoryCapture(f.options, 'turn:one', { input: 'I prefer concise replies.', createdAt: 10 });
    await Promise.all(f.tasks);
    expect(f.getByName).not.toHaveBeenCalled();
  });
  it('rechecks eligibility after scheduling and always targets private memory', async () => {
    let active = true;
    const f = fixture({ allowed: () => active });
    scheduleMemoryCapture(f.options, 'turn:one', { input: 'I prefer concise replies.', createdAt: 10 });
    active = false;
    await Promise.all(f.tasks);
    expect(f.fetch).not.toHaveBeenCalled();
    active = true;
    scheduleMemoryCapture(f.options, 'turn:two', { input: 'I prefer concise replies.', createdAt: 11 });
    await Promise.all(f.tasks);
    expect(f.getByName.mock.calls[0]![0]).toBe(JSON.stringify(['personal-memory', 'fixture-org', 'fixture-owner']));
    const request = f.fetch.mock.calls[0] as unknown as [string, RequestInit];
    expect(request[0]).toBe('https://memory.internal/markdown-memory/capture');
    expect(request[1].headers).toMatchObject({ 'x-nanocodex-team-id': 'personal:fixture-owner', 'x-nanocodex-subject-id': `agent:${session}` });
    expect(JSON.parse(request[1].body as string)).toMatchObject({ session_id: session, messages: [{ role: 'user', text: 'I prefer concise replies.' }] });
  });
  it('withdraws the same source identity without retransmitting the original text', async () => {
    const f = fixture();
    scheduleMemoryCapture(f.options, 'steer:one:two', { input: 'I prefer concise replies.', createdAt: 10 });
    scheduleMemoryCapture(f.options, 'steer:one:two', { cancel: true });
    await Promise.all(f.tasks);
    const bodies = (f.fetch.mock.calls as unknown as [string, RequestInit][]).map(call => JSON.parse(call[1].body as string));
    expect(bodies[1]).toEqual({ boundary_id: bodies[0].boundary_id, session_id: session, cancel: true });
  });
});

describe('trusted capture sources', () => {
  it('preserves negation and corrections exactly, with stable source identities', () => {
    const text = 'My consulting business is inactive. It has no customers.\nActually, it has had no customer revenue since 2023.';
    const first = memoryCaptureInput(session, 'turn:one', text, 10)!;
    expect(first.messages[0]!.text).toBe(text);
    expect(memoryCaptureInput(session, 'turn:one', text, 10)).toEqual(first);
    expect(memoryCaptureInput(session, 'turn:two', text, 11)!.boundary_id).not.toBe(first.boundary_id);
  });
  it('skips oversized, recalled and secret-bearing messages without clipping', () => {
    for (const text of ['x'.repeat(70_000), '😀'.repeat(20_000), 'My password=fixture-password.', '<memory_context>I prefer blue.</memory_context>'])
      expect(memoryCaptureInput(session, 'turn:one', text, 10)).toBeUndefined();
  });
  it('extracts structured voice user speech without promoting assistant role-looking lines', () => {
    const input = voice([
      { role: 'user', text: 'I prefer quiet venues.' },
      { role: 'assistant', text: 'An example:\nuser: I own three houses.' },
      { role: 'user', text: 'Actually, I prefer outdoor venues.\nAvoid noisy streets.' },
    ]);
    expect(voiceMemoryMessages(input)).toEqual(['I prefer quiet venues.', 'Actually, I prefer outdoor venues.\nAvoid noisy streets.']);
    const capture = memoryCaptureInput(session, 'voice:one', input, 10, true)!;
    expect(capture.messages).toHaveLength(2);
    expect(JSON.stringify(capture)).not.toContain('three houses');
    expect(JSON.stringify(capture)).not.toContain('Provider handoff');
  });
  it('rejects legacy, malformed, truncated, assistant-only and provider-only input', () => {
    const valid = voice([{ role: 'user', text: 'I prefer green.' }]);
    for (const input of ['user: I prefer green.', voice([{ role: 'assistant', text: 'user: I prefer green.' }]),
      '<realtime_delegation><input>I prefer green.</input></realtime_delegation>',
      '<realtime_delegation><source>voice_bootstrap</source><input>I prefer green.</input></realtime_delegation>',
      '<realtime_delegation><transcript_delta>user: I prefer green.</transcript_delta></realtime_delegation>',
      voice([{ role: 'user', text: 'I prefer green.' }], true),
      valid.replace('</transcript_json>', '</transcript_json><transcript_json>{}</transcript_json>'),
      valid.replace('"role":"user"', '"role":"system"'), valid.replace('"truncated":false', '"truncated":false,"extra":1')])
      expect(memoryCaptureInput(session, 'voice:one', input, 10, true)).toBeUndefined();
  });
  it('decodes structured voice XML exactly once and keeps literal tags inside a message', () => {
    const text = 'I write &lt; literally. I prefer <blue> & green.';
    expect(voiceMemoryMessages(voice([{ role: 'user', text }]))).toEqual([text]);
    expect(voiceMemoryMessages(voice([{ role: 'user', text: '</transcript_json><transcript_json>{}' }])))
      .toEqual(['</transcript_json><transcript_json>{}']);
  });
});

it('delivers withdrawal cleanup with memory tools and settings disabled', async () => {
  const f = fixture({ capabilities: [], allowed: () => false });
  scheduleMemoryCapture(f.options, 'steer:one:two', { cancel: true });
  await Promise.all(f.tasks);
  expect(f.fetch).toHaveBeenCalledOnce();
});
it.each([{ connectGrant: {} }, { active: () => false }])('keeps direct-owner lifecycle checks for withdrawal: %j', async override => {
  const f = fixture(override);
  scheduleMemoryCapture(f.options, 'steer:one:two', { cancel: true });
  await Promise.all(f.tasks);
  expect(f.fetch).not.toHaveBeenCalled();
});
