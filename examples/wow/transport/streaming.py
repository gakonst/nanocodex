"""Persistable projection of managed protocol events into <=4096-byte S bodies.

Source: js/managed/src/protocol.ts (event wrapper/turn_id),
js/nanocodex-react/agent/transcript.mjs (assistant.delta/message identity).
Wire: ncs1 TAB request TAB turn TAB stream TAB revision TAB byte_offset TAB op
LF raw UTF-8. Each operation advances revision; reset clears text at offset 0.
Offsets count UTF-8 bytes. Receiver must reject gaps, ignore old revisions, and
accept a higher-revision reset for canonical snapshot recovery. No JSON needed.
"""
import hashlib
import json
import re

MAX_STREAM_MESSAGE = 4096
SAFE_ID = re.compile(r'^[A-Za-z0-9._:-]{1,128}$')
OPS = {'append', 'reset', 'end', 'error', 'done'}
MAX_TEXT = 256 * 1024

class StreamLimit(ValueError):
    pass


def header(request, turn, stream, revision, offset, op):
    if any(not isinstance(x, str) or not SAFE_ID.fullmatch(x) for x in (request, turn, stream)):
        raise ValueError('Invalid stream identity.')
    if type(revision) is not int or revision < 1 or type(offset) is not int or offset < 0 or op not in OPS:
        raise ValueError('Invalid stream position.')
    return f'ncs1\t{request}\t{turn}\t{stream}\t{revision}\t{offset}\t{op}\n'


def decode(value):
    """Strict reference decoder, also useful to test independently of Lua."""
    if isinstance(value, bytes):
        value = value.decode('utf-8')
    if not isinstance(value, str) or len(value.encode('utf-8')) > MAX_STREAM_MESSAGE:
        raise ValueError('Stream body exceeds limit.')
    first, text = value.split('\n', 1)
    version, request, turn, stream, rev, offset, op = first.split('\t')
    if version != 'ncs1' or not rev.isascii() or not rev.isdecimal() or not offset.isascii() or not offset.isdecimal():
        raise ValueError('Invalid stream header.')
    rev, offset = int(rev), int(offset)
    header(request, turn, stream, rev, offset, op)
    if (op != 'append' and text) or (op == 'reset' and offset):
        raise ValueError('Invalid stream operation.')
    return dict(request=request, turn=turn, stream=stream, revision=rev, offset=offset, op=op, text=text)


def _stream(record, identity):
    key = hashlib.sha256(json.dumps(identity, separators=(',', ':'), ensure_ascii=True).encode()).hexdigest()[:32]
    streams = record.setdefault('streams', {})
    if key not in streams:
        if len(streams) >= 32:
            raise StreamLimit('Too many assistant blocks.')
        streams[key] = dict(text='', revision=0, offset=0, sealed=False, identity=identity)
    return key, streams[key]


def _emit(rid, record, key, stream, op, text=''):
    """Split only on UTF-8 boundaries, including the envelope in the bound."""
    raw = text.encode('utf-8')
    if op == 'append' and sum(len(s['text'].encode('utf-8')) for s in record['streams'].values()) + len(raw) > MAX_TEXT:
        raise StreamLimit('Answer exceeds the 256 KiB display limit.')
    if op == 'reset':
        stream.update(text='', offset=0, sealed=False)
    while True:
        revision = stream['revision'] + 1
        prefix = header(rid, record['turn'], key, revision, stream['offset'], op)
        capacity = MAX_STREAM_MESSAGE - len(prefix.encode())
        chunk = raw[:capacity]
        if len(chunk) < len(raw):
            chunk = chunk.decode('utf-8', errors='ignore').encode('utf-8')
        part = chunk.decode('utf-8')
        value = prefix + part
        record['outputs'].append(dict(kind='stream', value=value, request_id=rid,
            state='error' if op == 'error' else 'completed' if op == 'done' else 'streaming',
            event_id=f'{rid}:stream:{key}:{revision}'))
        stream['revision'] = revision
        stream['offset'] += len(chunk)
        stream['text'] += part
        raw = raw[len(chunk):]
        if not raw:
            break
    if op in ('end', 'error'):
        stream['sealed'] = True


def _snapshot(rid, record, key, stream, text):
    if not isinstance(text, str):
        raise ValueError('Invalid assistant snapshot.')
    if text == stream['text']:
        return
    if text.startswith(stream['text']) and not stream['sealed']:
        _emit(rid, record, key, stream, 'append', text[len(stream['text']):])
    else:
        _emit(rid, record, key, stream, 'reset')
        if text:
            _emit(rid, record, key, stream, 'append', text)


def project(rid, record, event):
    """Mutate journal record. Caller commits cursor and outputs atomically.

    Source cursor dedup is performed by Dispatcher, not inferred from item seq:
    seq may include invisible tool/reasoning events and agent-local sequences.
    """
    kind = event.get('type')
    if kind == 'event':
        if event.get('agent_id') is not None:  # Child-agent text is not the root reply.
            return
        inner = event.get('event') or {}
        if inner.get('type') not in ('assistant.delta', 'assistant.message'):
            return
        payload = inner.get('payload') or {}
        text = payload.get('text')
        if not isinstance(text, str):
            raise ValueError('Invalid assistant text.')
        identity = [payload.get('model_call_index'), payload.get('item_id'), payload.get('phase')]
        key, stream = _stream(record, identity)
        if payload.get('phase') != 'commentary':
            record['answer_stream'] = key
        if inner['type'] == 'assistant.message':
            _snapshot(rid, record, key, stream, text)
            stream['canonical'] = True
        elif not stream.get('canonical') and not stream['sealed'] and text:
            _emit(rid, record, key, stream, 'append', text)
    elif kind == 'turn_completed':
        text = event.get('final_message')
        # Completion may omit a canonical snapshot (or carry null). Only an
        # actual string supersedes accumulated deltas/messages; '' explicitly
        # means the canonical answer is empty.
        if text is not None and not isinstance(text, str):
            raise ValueError('Invalid final text.')
        key = record.get('answer_stream')
        if key:
            stream = record['streams'][key]
        else:
            key, stream = _stream(record, [None, None, 'final_answer'])
        if text is not None:
            _snapshot(rid, record, key, stream, text)
        final_key, final_stream = key, stream
        for key, stream in record['streams'].items():
            if not stream['sealed']:
                _emit(rid, record, key, stream, 'end')
        _emit(rid, record, final_key, final_stream, 'done')
        record['done'] = True
    elif kind in ('turn_failed', 'turn_cancelled', 'stream_failed'):
        if not record.get('streams'):
            _stream(record, [None, None, 'final_answer'])
        for key, stream in record['streams'].items():
            if not stream['sealed']:
                _emit(rid, record, key, stream, 'error')
        record['done'] = True
