"""Headless application boundary; no hardware, HTTP listener, Lua or credential IO.

Driver hookup (run backend IO on a worker, never in Pump.tick):
    backend = DurableBackend()  # imported from durable_client; CLI account store only
    dispatcher = Dispatcher(backend, private_journal_path)
    outputs = dispatcher.dispatch(complete_utf8_json, request_id=stable_message_id)
    outputs = dispatcher.poll(stable_message_id)  # read-only progress reconciliation
    outputs = dispatcher.projects(stable_refresh_id)  # explicit roster refresh
    status = dispatcher.status()  # connected only on actual /api/status evidence

Bridge.lua currently omits IDs. The application transport MUST assign and retain
one stable_id-compatible ID before first delivery (across reassembly/reconnect),
pass it separately, and never derive it from frame sequence alone. An optional
JSON request_id must match. Missing IDs are rejected, never randomly replaced.
Outputs have kind,value,request_id,state,event_id. R/P/A/E values are complete plain UTF-8 text (<=16384 bytes). S values are
ncs1 incremental stream envelopes (<=4096 bytes including header); long answers
use multiple S messages without truncation. See transport.streaming for schema.
States are local_queued, remoteaccepted, reply, completed, error, or unknown.
Carrier ACK and local enqueue are not remote admission or model completion.

Use transport.messages.Assembler(deliver, accepted_kinds=('Q',)) between Link
and dispatch. Its callback receives ('request', complete_bytes), not the wire Q
letter. Only then dispatch decodes UTF-8 and JSON. No fragment is decoded alone.
MAX_REQUEST imports the codec's MAX_MESSAGE: count serialized UTF-8 JSON bytes,
including escaping/context, not prompt characters. Oversized requests are
rejected before backend IO. Preserve request_id and exact input across retries.

For each output, iterate wire_fragments(output, wire_message_id), feeding the
next fragment to Link.send as soon as the previous carrier frame is ACKed.
This delegates to the owner's fragments(kind, id, text): M + R/P/A/E + u16 ID,
total, offset + <=88 bytes. The transport reassembles ONE complete message before
NS.OnTransportMessage(kind, text), so P always carries a complete ncw1 snapshot.
encode_output returns UTF-8 body bytes; S has a tab header followed by raw text.
Request/event identities stay in the adapter/journal, not the display text.
Retain outputs until carrier delivery; repeated calls replay stable event_ids
for adapter deduplication. The burst Pump supplies pacing; no sleeps are added.
This is a local application API, not live transport or full app readiness proof.

The private SQLite journal commits intent before backend mutation. Duplicate IDs
replay receipts, conflicting input fails, and interrupted/uncertain mutations
are NEVER resubmitted here, including after restart. A caller must reconcile an
unknown outcome explicitly, not assign a fresh ID to retry it. DurableBackend
alone owns its supported WebSocket outbox/replay behavior. Journal capacity is
bounded and fails closed (no eviction of mutation identities). Keep one journal
per backend account/origin, rotate only after reconciliation; never share across
accounts. close() closes the journal, not the caller-owned backend.
"""
import copy
import hashlib
import json
import re
import threading
from urllib.parse import quote

from durable_client import EventStore
from server import APIError, MODES, display_name, identifier, stable_id

from transport.messages import MAX_MESSAGE, fragments
from transport.streaming import project, decode as decode_stream, StreamLimit

MAX_REQUEST = MAX_MESSAGE
MAX_VALUE = MAX_MESSAGE
WIRE_KINDS = {'reply': 'R', 'projects': 'P', 'ack': 'A', 'error': 'E', 'stream': 'S'}
MAX_RECORDS = 1024
MAX_ROWS = 1000


class InvalidRequest(ValueError):
    pass


class RequestTooLarge(InvalidRequest):
    pass


def _text(value, limit, *, controls=False):
    if not isinstance(value, str) or len(value.encode('utf-8')) > limit:
        raise InvalidRequest('Invalid text size.')
    if controls and re.search(r'[\x00-\x1f\x7f]', value):
        raise InvalidRequest('Control characters are not allowed.')
    return value


def _pairs(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise InvalidRequest('Duplicate JSON field.')
        value[key] = item
    return value


def _tree(value, depth=0):
    if depth > 12:
        raise InvalidRequest('JSON nesting exceeds limit.')
    if isinstance(value, dict):
        for key, item in value.items():
            _text(key, 256)
            _tree(item, depth + 1)
    elif isinstance(value, list):
        if len(value) > 1000:
            raise InvalidRequest('JSON array exceeds limit.')
        for item in value:
            _tree(item, depth + 1)
    elif isinstance(value, str):
        _text(value, MAX_REQUEST)


def parse_request(payload, request_id=None):
    """Strict complete-message parser. Raises InvalidRequest; performs no IO."""
    try:
        if isinstance(payload, (str, bytes)):
            size = len(payload.encode('utf-8')) if isinstance(payload, str) else len(payload)
            if size > MAX_REQUEST:
                raise RequestTooLarge('Request exceeds 16 KiB application limit.')
        if isinstance(payload, bytes):
            payload = payload.decode('utf-8')
        _text(payload, MAX_REQUEST)
        value = json.loads(payload, object_pairs_hook=_pairs,
                           parse_constant=lambda _: (_ for _ in ()).throw(InvalidRequest('Nonfinite JSON.')))
        _tree(value)
        if not isinstance(value, dict):
            raise InvalidRequest('Expected JSON object.')
        if type(value.get('schemaVersion')) is not int or value['schemaVersion'] != 1 or value.get('source') != 'nanocodex-wow':
            raise InvalidRequest('Unsupported schema or source.')
        rid = stable_id(request_id if request_id is not None else value.get('request_id'))
        if 'request_id' in value and stable_id(value['request_id']) != rid:
            raise InvalidRequest('Request ID mismatch.')
        base = {'schemaVersion', 'source', 'type', 'request_id'}
        kind = value.get('type')
        if kind == 'nanocodex.ask':
            if set(value) - base - {'prompt', 'mode', 'project_id', 'thread_id', 'context'}:
                raise InvalidRequest('Unsupported ask fields.')
            prompt = _text(value.get('prompt'), 64000)
            if not prompt.strip() or len(prompt) > 16000 or value.get('mode') not in MODES:
                raise InvalidRequest('Invalid prompt or mode.')
            data = {'text': prompt, 'mode': value['mode'], 'model': 'luna', 'idempotency_key': rid}
            for key in ('project_id', 'thread_id'):
                if key in value:
                    data[key] = identifier(value[key])
            if 'context' in value:
                if not isinstance(value['context'], (dict, str)):
                    raise InvalidRequest('Invalid context.')
                data['context'] = value['context']
            route = '/api/send'
        elif kind == 'nanocodex.action':
            action = value.get('action')
            specs = {
                'create_project': ('/api/projects/create', {'name'}, set()),
                'create_chat': ('/api/threads/create', {'name', 'project_id'}, set()),
                'rename_project': ('/api/projects/rename', {'name', 'project_id'}, set()),
                'rename_chat': ('/api/threads/rename', {'name', 'thread_id'}, {'project_id'}),
                'refresh_projects': ('/api/projects', set(), set()),
            }
            if not isinstance(action, str) or action not in specs:
                raise InvalidRequest('Unsupported action.')
            route, required, optional = specs[action]
            if not required <= value.keys() or set(value) - base - {'action'} - required - optional:
                raise InvalidRequest('Invalid action fields.')
            data = {}
            for key in ('project_id', 'thread_id'):
                if key in value:
                    data[key] = identifier(value[key])
            if action == 'rename_chat':
                data.pop('project_id', None)  # UI selection context, not a backend field.
            if 'name' in value:
                data['title' if action.endswith('chat') else 'name'] = display_name(_text(value['name'], 512, controls=True))
            if action.startswith('create_'):
                data['idempotency_key'] = rid
        else:
            raise InvalidRequest('Unsupported request type.')
        canonical = json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False)
        return rid, route, data, hashlib.sha256(canonical.encode()).hexdigest()
    except RequestTooLarge:
        raise
    except (APIError, UnicodeError, ValueError, TypeError, RecursionError, OverflowError):
        raise InvalidRequest('Invalid application request, fields, or stable ID.') from None


def _receipt_id(value, *, turn=False):
    try:
        return stable_id(value) if turn else identifier(value)
    except APIError:
        raise InvalidRequest('Malformed backend receipt.') from None


def _output(rid, kind, value, state):
    _text(value, MAX_VALUE)
    return {'kind': kind, 'value': value, 'request_id': rid, 'state': state,
            'event_id': rid + ':' + state}


def encode_output(output):
    """Return complete plain UTF-8 wire body, excluding local receipt metadata."""
    if set(output) != {'kind', 'value', 'request_id', 'state', 'event_id'} or output['kind'] not in WIRE_KINDS:
        raise InvalidRequest('Invalid output envelope.')
    if output['kind'] == 'stream':
        decode_stream(output['value'])
    return _text(output['value'], MAX_VALUE).encode('utf-8')


def wire_fragments(output, message_id):
    """Use the owner's codec; no independent framing, pacing, or input actions."""
    body = encode_output(output)
    return fragments(WIRE_KINDS[output['kind']], message_id, body)


def _visible(outputs):
    # Validate complete retained outputs too, including receipts read after restart.
    for output in outputs:
        encode_output(output)
    return outputs


def _error(rid, status=502):
    messages = {401: 'Sign in with nanocodex2 login on this machine.',
                403: 'Account access denied.', 409: 'Request ID conflict.',
                400: 'Invalid application request.', 413: 'Dispatcher capacity exceeded.'}
    # Never echo exception text, raw backend responses, or credentials.
    return _output(rid, 'error', messages.get(status, 'Outcome could not be confirmed. Reconcile before resubmitting.'),
                   'error' if status in (400, 401, 403, 409, 413) else 'unknown')


class Dispatcher:
    def __init__(self, backend, journal_path):
        self.backend = backend
        self.journal = EventStore(journal_path)
        self.lock = threading.RLock()
        with self.journal.db:
            self.journal.db.execute('CREATE TABLE IF NOT EXISTS dispatch_requests (id TEXT PRIMARY KEY, digest TEXT NOT NULL, record TEXT NOT NULL)')

    def close(self):
        self.journal.close()

    def _read(self, rid):
        row = self.journal.db.execute('SELECT digest, record FROM dispatch_requests WHERE id=?', (rid,)).fetchone()
        return (row[0], json.loads(row[1])) if row else None

    def _save(self, rid, record):
        with self.journal.db:
            self.journal.db.execute('UPDATE dispatch_requests SET record=? WHERE id=?', (json.dumps(record), rid))

    def dispatch(self, payload, request_id=None):
        """Return receipts; never retries an already journaled backend operation."""
        try:
            rid, route, data, digest = parse_request(payload, request_id)
        except InvalidRequest as exc:
            try:
                rid = stable_id(request_id)
            except APIError:
                rid = 'invalid'
            if isinstance(exc, RequestTooLarge):
                return [_output(rid, 'error', 'Request exceeds 16 KiB application limit; nothing was submitted.', 'error')]
            return [_error(rid, 400)]
        with self.lock:
            # BEGIN IMMEDIATE also serializes intent admission across processes.
            with self.journal.db:
                self.journal.db.execute('BEGIN IMMEDIATE')
                saved = self._read(rid)
                if saved:
                    return _visible(saved[1]['outputs']) if saved[0] == digest else [_error(rid, 409)]
                if self.journal.db.execute('SELECT count(*) FROM dispatch_requests').fetchone()[0] >= MAX_RECORDS:
                    return [_error(rid, 413)]
                record = {'outputs': [_error(rid)], 'thread': None, 'turn': None, 'after': '0', 'done': True}
                self.journal.db.execute('INSERT INTO dispatch_requests VALUES(?,?,?)', (rid, digest, json.dumps(record)))
            try:
                if route == '/api/projects':
                    record['outputs'] = [self._projects(rid)]
                else:
                    result = self.backend.handle('POST', route, {}, data)
                    if route == '/api/send':
                        thread, turn = _receipt_id(result.get('thread_id')), _receipt_id(result.get('turn_id'), turn=True)
                        status = result.get('status')
                        if status not in ('queued', 'accepted'):
                            raise InvalidRequest('Unconfirmed receipt.')
                        state = 'local_queued' if status == 'queued' else 'remoteaccepted'
                        message = 'Queued locally; awaiting remote acceptance.' if status == 'queued' else 'Accepted remotely; awaiting reply.'
                        record.update(thread=thread, turn=turn, done=False)
                    else:
                        _receipt_id(result.get('project_id' if '/projects/' in route else 'thread_id'))
                        name_field = 'name' if '/projects/' in route else 'title'
                        if result.get(name_field) != data[name_field]:
                            raise InvalidRequest('Unconfirmed organization receipt.')
                        if route.endswith('/create') and result.get('status') != 'created':
                            raise InvalidRequest('Unconfirmed creation receipt.')
                        state, message = 'completed', 'Organization action completed.'
                    record['outputs'] = [_output(rid, 'ack', message, state)]
            except Exception as exc:
                record['outputs'] = [_error(rid, exc.status if isinstance(exc, APIError) else 502)]
            self._save(rid, record)
            return _visible(record['outputs'])

    def projects(self, request_id):
        """Explicit read-only ncw1 refresh, deduplicated by its stable request ID."""
        return self.dispatch(json.dumps({'schemaVersion': 1, 'source': 'nanocodex-wow',
                             'type': 'nanocodex.action', 'action': 'refresh_projects'}), request_id)

    def _projects(self, rid):
        projects = self.backend.handle('GET', '/api/projects', {}, {})['projects']
        if not isinstance(projects, list) or len(projects) > MAX_ROWS:
            raise InvalidRequest('Too many projects.')
        rows, seen_projects, seen_threads = ['ncw1'], set(), set()
        snapshot_bytes = 4
        def add(fields):
            nonlocal snapshot_bytes
            if len(rows) > MAX_ROWS:
                raise InvalidRequest('Too many rows.')
            row = fields[0] + '\t' + '\t'.join(quote(_text(s, 4096, controls=True), safe='') for s in fields[1:])
            snapshot_bytes += 1 + len(row.encode('utf-8'))
            if snapshot_bytes > MAX_VALUE:
                raise APIError('Snapshot exceeds retained-content limit.', 413)
            rows.append(row)
        for project in projects:
            pid = identifier(project['id'])
            if pid in seen_projects:
                raise InvalidRequest('Duplicate project.')
            seen_projects.add(pid)
            add(['P', pid, project['name']])
            threads = self.backend.handle('GET', '/api/threads', {'project_id': [pid]}, {})['threads']
            if not isinstance(threads, list) or len(threads) > MAX_ROWS:
                raise InvalidRequest('Too many threads.')
            for thread in threads:
                tid = identifier(thread['id'])
                if tid in seen_threads:
                    raise InvalidRequest('Duplicate thread.')
                seen_threads.add(tid)
                add(['T', pid, tid, thread['title'], thread.get('status', 'unknown')])
        return _output(rid, 'projects', '\n'.join(rows), 'completed')

    def status(self):
        """Safe status projection. Never infer account connectivity from carrier IO."""
        try:
            result = self.backend.handle('GET', '/api/status', {}, {})
            connected = result.get('connected') is True
        except Exception:
            connected = False
        return {'connected': connected, 'state': 'connected' if connected else 'disconnected'}

    def poll(self, request_id):
        """Project committed WebSocket events; stubs retain history compatibility.

        Real DurableBackend uses only store.events here, never full-answer polls.
        Subscription belongs to DurableBackend; restart restores it lazily.
        Outputs and consumed cursor commit together for stable replay identities.
        """
        rid = stable_id(request_id)
        with self.lock:
            saved = self._read(rid)
            if not saved:
                return [_error(rid, 400)]
            record = copy.deepcopy(saved[1])
            if record['done'] or not record['thread']:
                return _visible(record['outputs'])
            try:
                # Prefer committed durable events, which can be consumed offline.
                store = getattr(self.backend, 'store', None)
                if store is not None:
                    from durable_client import cursor
                    events = store.events(record['thread'], record.get('durable_after', '0'), 256)
                    # Coalesce adjacent visible deltas from one assistant block.
                    # Transport latency must not turn every token into a packet.
                    merged = []
                    seen_position = cursor(record.get('durable_after', '0'))
                    for source_event in events:
                        position = cursor(source_event['cursor'])
                        if position <= seen_position:
                            continue
                        seen_position = position
                        event = copy.deepcopy(source_event)
                        inner = event.get('event') or {}
                        payload = inner.get('payload') or {}
                        signature = (event.get('turn_id'), event.get('agent_id'), payload.get('model_call_index'), payload.get('item_id'), payload.get('phase'))
                        if merged and event.get('type') == 'event' and inner.get('type') == 'assistant.delta' and isinstance(payload.get('text'), str):
                            prior = merged[-1]
                            pi = prior.get('event') or {}; pp = pi.get('payload') or {}
                            ps = (prior.get('turn_id'), prior.get('agent_id'), pp.get('model_call_index'), pp.get('item_id'), pp.get('phase'))
                            if prior.get('type') == 'event' and pi.get('type') == 'assistant.delta' and ps == signature and isinstance(pp.get('text'), str):
                                pp['text'] += payload['text']; prior['cursor'] = event['cursor']
                                continue
                        merged.append(event)
                    for event in merged:
                        position = cursor(event['cursor'])
                        if position <= cursor(record.get('durable_after', '0')):
                            continue
                        record['durable_after'] = str(position)
                        kind = event.get('type')
                        turn = event.get('turn_id') or event.get('id')
                        if turn != record['turn'] and kind != 'stream_failed':
                            continue
                        if kind == 'turn_accepted':
                            self._progress(rid, record, kind, None)
                        else:
                            project(rid, record, event)
                        if record['done']:
                            break
                    self._save(rid, record)
                    # Restore each waiting thread independently after restart.
                    # The first subscription opens the shared store, so store
                    # presence alone cannot prove this thread is subscribed.
                    # Consume cached completion before requiring connectivity.
                    subscribe = getattr(self.backend, 'subscribe', None)
                    clients = getattr(self.backend, 'clients', None)
                    if not record['done'] and subscribe is not None and clients is not None and record['thread'] not in clients:
                        try:
                            subscribe(record['thread'])
                        except Exception as exc:
                            return _visible(record['outputs']) + [_error(rid, exc.status if isinstance(exc, APIError) else 502)]
                    return _visible(record['outputs'])
                # A restarted DurableBackend may not have opened its event store.
                subscribe = getattr(self.backend, 'subscribe', None)
                if subscribe is not None:
                    subscribe(record['thread'])
                    return _visible(record['outputs'])
                result = self.backend.handle('GET', '/api/messages',
                    {'thread_id': [record['thread']], 'after': [record['after']], 'limit': ['256']}, {})
                details = result['message_details']
                if not isinstance(details, list) or len(details) > 256:
                    raise InvalidRequest('Invalid history.')
                for event in details:
                    if event.get('turn_id') == record['turn']:
                        self._progress(rid, record, event.get('event_type'), event.get('text'))
                        if record['done']:
                            break
                last = result.get('last_cursor')
                if last is not None:
                    from durable_client import cursor
                    if cursor(last) < cursor(record['after']):
                        raise InvalidRequest('History cursor regressed.')
                    record['after'] = last
                self._save(rid, record)
                return _visible(record['outputs'])
            except StreamLimit:
                output = _output(rid, 'error', 'Answer exceeds the in-game display limit; open it in Nanocodex or request a shorter answer.', 'error')
                output['event_id'] = rid + ':stream-limit'
                record['outputs'].append(output); record['done'] = True
                self._save(rid, record)
                return _visible(record['outputs'])
            except Exception as exc:
                # Progress remains replayable; errors do not authorize resubmission.
                return _visible(saved[1]['outputs']) + [_error(rid, exc.status if isinstance(exc, APIError) else 502)]

    @staticmethod
    def _progress(rid, record, kind, text):
        if kind == 'turn_completed':
            if isinstance(text, str) and len(text.encode('utf-8')) > MAX_VALUE:
                project(rid, record, {'type': 'turn_completed', 'final_message': text})
                return
            _text(text, MAX_VALUE)  # Validate before changing acceptance state.
        if kind in ('turn_accepted', 'turn_completed') and not any(x['state'] == 'remoteaccepted' for x in record['outputs']):
            record['outputs'].append(_output(rid, 'ack', 'Accepted remotely; awaiting reply.', 'remoteaccepted'))
        if kind == 'turn_completed':
            record['outputs'].append(_output(rid, 'reply', text, 'reply'))
            record['done'] = True
        elif kind in ('turn_failed', 'turn_cancelled'):
            record['outputs'].append(_output(rid, 'error', 'Turn failed or was cancelled.', 'error'))
            record['done'] = True
