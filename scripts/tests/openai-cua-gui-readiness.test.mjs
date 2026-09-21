import test from 'node:test';
import assert from 'node:assert/strict';
import { KNOWN_GUI_BUILD, MAX_GUI_READY_LINE_BYTES, parseGuiReady, createGuiReadiness } from '../../crates/experimental/nanocodex-computer/src/openai-cua-gui-readiness.mjs';

const id = '00000000-0000-7000-8000-000000000001';
const other = '00000000-0000-7000-8000-000000000002';
const prefix = '[electron-message-handler] maybe_resume_success ';
const fields = `assignedStreamRole=owner conversationId=${id} markedStreaming=true threadId=${id} vmEvent=thread_resumed`;
const line = prefix + fields;

test('accepts the pinned event with fully parsed logger scalar and JSON fields', () => {
  assert.equal(KNOWN_GUI_BUILD, '9922');
  assert.equal(parseGuiReady(line, id), true);
  assert.equal(parseGuiReady(`${line} routePath="/local/a b" environmentIds=["x", "y"] extra={"message":"a \\\"quoted\\\" value", "nested":[1,true]} count=0 absent=undefined`, id), true);
  assert.equal(parseGuiReady(`${prefix}assignedStreamRole="owner" conversationId="${id}" markedStreaming=true threadId="${id}" vmEvent="thread_resumed"`, id), true);
});

test('requires exact framing, UUID, role, event and boolean', () => {
  for (const invalid of [
    ` ${line}`, `info ${line}`, `other ${line}`, line + '\n', line + '\r', line + ' ',
    line.replace('maybe_resume_success', 'maybe_resume_success_extra'),
    line.replace('assignedStreamRole=owner', 'assignedStreamRole=follower'),
    line.replace('vmEvent=thread_resumed', 'vmEvent=thread_started'),
    line.replace('markedStreaming=true', 'markedStreaming="true"'),
    line.replace(`conversationId=${id}`, `conversationId=${other}`),
    line.replace(`threadId=${id}`, `threadId=${other}`),
    line.replace('markedStreaming=true ', ''),
  ]) assert.equal(parseGuiReady(invalid, id), false, invalid);
  for (const expected of ['', 'not-a-uuid', null, 5, other]) assert.equal(parseGuiReady(line, expected), false);
});

test('quoted spoof fields and escaped newlines cannot supply required fields', () => {
  assert.equal(parseGuiReady(`${prefix}message=${JSON.stringify(fields)}`, id), false);
  assert.equal(parseGuiReady(`${prefix}message=${JSON.stringify(`\n${line}`)}`, id), false);
  assert.equal(parseGuiReady(`${prefix}payload={"message":${JSON.stringify(fields)}}`, id), false);
  assert.equal(parseGuiReady(`${line} message=${JSON.stringify(`threadId=${other}\n${line}`)}`, id), true);
  assert.equal(parseGuiReady(`${prefix}message="ok"\n${line}`, id), false);
  assert.equal(parseGuiReady(`${prefix}message="ok"\u2028${line}`, id), false);
});

test('rejects every duplicate key and malformed or trailing material', () => {
  for (const suffix of [
    ` threadId=${id}`, ` conversationId=${id}`, ' assignedStreamRole=owner',
    ' x=one x=two', ' x="unterminated', ' x="ok"junk', ' x=[1,]', ' x={"a":1]',
    ' x={"a":}', ' x=', ' x=a=b', ' x=bad\\value', ' invalid-key!', '  x=1',
    ' x=true trailing', ' x="bad\\q"',
  ]) assert.equal(parseGuiReady(line + suffix, id), false, suffix);
});

test('enforces byte size rather than character size', () => {
  const start = line + ' padding=';
  assert.equal(parseGuiReady(start + 'x'.repeat(MAX_GUI_READY_LINE_BYTES - Buffer.byteLength(start)), id), true);
  assert.equal(parseGuiReady(start + 'x'.repeat(MAX_GUI_READY_LINE_BYTES), id), false);
  assert.equal(parseGuiReady(`${line} padding="${'🌍'.repeat(MAX_GUI_READY_LINE_BYTES / 4)}"`, id), false);
});

function observer() {
  const ready = [];
  const generation = {};
  const reader = createGuiReadiness({ generation, onReady: value => ready.push(value) });
  reader.expect(id);
  const push = (chunk, options = {}) => reader.push(chunk, { channel: 'stdout', generation, ...options });
  return { ready, reader, push };
}

test('stream accepts only complete stdout lines from its exact owned generation', () => {
  const { ready, push } = observer();
  push(line + '\n', { channel: 'stderr' });
  push(line + '\n', { generation: {} });
  assert.deepEqual(ready, []);
  push(line.slice(0, 45));
  push(line.slice(45));
  assert.deepEqual(ready, []);
  push('\n');
  push(line + '\n');
  assert.deepEqual(ready, [id]);
});

test('foreign fragments do not affect current stdout framing', () => {
  const { ready, push } = observer();
  push(line.slice(0, 20));
  push('malicious\n' + line + '\n', { channel: 'stderr' });
  push('malicious\n' + line + '\n', { generation: 'old' });
  push(line.slice(20) + '\n');
  assert.deepEqual(ready, [id]);
});

test('oversize line suffix is discarded through newline and subsequent line can match', () => {
  const { ready, push } = observer();
  push('x'.repeat(MAX_GUI_READY_LINE_BYTES + 1));
  push(line + '\n');
  assert.deepEqual(ready, []);
  push(line + '\n');
  assert.deepEqual(ready, [id]);
});

test('UTF-8 chunk boundaries preserve quoted fields', () => {
  const { ready, push } = observer();
  const bytes = Buffer.from(`${line} note="🌍"\n`);
  for (const byte of bytes) push(Buffer.from([byte]));
  assert.deepEqual(ready, [id]);
});

test('new expectations cannot consume prior partial lines and close is permanent', () => {
  const { ready, reader, push } = observer();
  push(line.slice(0, 40));
  reader.expect(id);
  push(line + '\n');
  assert.deepEqual(ready, []);
  push(line + '\n');
  assert.deepEqual(ready, [id]);
  reader.close();
  push(line + '\n');
  assert.deepEqual(ready, [id]);
  assert.throws(() => reader.expect(id), /closed/);
});

test('never combines partial evidence across observer generations or EOF', () => {
  const first = observer();
  first.push(line.slice(0, 30));
  first.reader.close();
  first.push(line.slice(30) + '\n');
  const second = observer();
  second.push(line.slice(30) + '\n');
  second.push(line); // EOF is not a complete log record.
  second.reader.close();
  assert.deepEqual(first.ready, []);
  assert.deepEqual(second.ready, []);
});
