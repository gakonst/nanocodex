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
States are local_queued, remoteaccepted, metadata, streaming, reply, completed, error, or unknown.
Tagged A metadata uses ncm1 with percent-encoded tab-separated fields. Sends
retain their human ACK followed by ncm1/send/request/thread/turn/local_queued|remoteaccepted;
metadata is not a terminal state. Creation emits ncm1/created/request/project/thread/title.
Fallback plain R replies are immediately preceded by ncm1/reply/request/thread/turn
so selection-aware clients can route completed answers without changing R framing.
refresh_projects with page=0 enables an immutable journal snapshot (4096 rows,
1 MiB, <=128 pages). Continue with snapshot_id and page, each under a new request
ID. A ncm1/projects/snapshot/page/pages precedes its P ncw1 partial row page;
combine all row pages before validating/importing the roster. The legacy action
without page retains its single-message behavior. Both open and locally closed
threads are included in the paged roster; closed rows carry status=closed. The
initial page reads GET /api/workspace once; later pages use only the journal.
load_history requires thread_id and stable view_id; optional before is a backend
history cursor. Its R body is nch1/snapshot/thread/page/pages/before/has_more/view
(tab-separated percent-encoded header) then newline and raw UTF-8 display text.
Continue the immutable page set with thread_id, view_id, snapshot_id and page.
Only after its last page use before for the next older backend batch (64 events).
stop_turn requires exact thread_id/turn_id and confirms only a cancellation
request. connection_status probes GET /api/status; reconnect first invokes the
backend's supported resume() if available. Neither action accepts credentials.
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
NS.OnTransportMessage(kind, text), so P carries one complete ncw1 page (or legacy snapshot).
encode_output returns UTF-8 body bytes; S has a tab header followed by raw text.
User-facing prose excludes request/event identities; tagged metadata carries routing IDs.
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
from server import APIError, MODES, display_name, identifier, stable_id, history_query

from transport.messages import MAX_MESSAGE, fragments
from transport.streaming import project, decode as decode_stream, StreamLimit

MAX_REQUEST = MAX_MESSAGE
MAX_VALUE = MAX_MESSAGE
WIRE_KINDS = {'reply': 'R', 'projects': 'P', 'ack': 'A', 'error': 'E', 'stream': 'S'}
MAX_RECORDS = 1024
MAX_ROWS = 4096
MAX_CATALOG_BYTES = 1024 * 1024
MAX_CATALOG_PAGES = 128
MAX_SNAPSHOT_BYTES = 256 * 1024
MAX_PAGES = 32
HISTORY_LIMIT = 64


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
                'create_chat': ('/api/threads/create', {'name'}, {'project_id'}),
                'rename_project': ('/api/projects/rename', {'name', 'project_id'}, set()),
                'rename_chat': ('/api/threads/rename', {'name', 'thread_id'}, {'project_id'}),
                'refresh_projects': ('/api/projects', set(), {'page', 'snapshot_id'}),
                'load_history': ('/api/messages', {'thread_id', 'view_id'}, {'before', 'page', 'snapshot_id'}),
                'stop_turn': ('/api/turns/cancel', {'thread_id', 'turn_id'}, set()),
                'connection_status': ('/api/status', set(), set()),
                'reconnect': ('/api/status', set(), set()),
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
            if 'turn_id' in value:
                data['turn_id'] = stable_id(value['turn_id'])
            if 'view_id' in value:
                data['view_id'] = stable_id(value['view_id'])
            if 'page' in value:
                page_limit = MAX_CATALOG_PAGES if action == 'refresh_projects' else MAX_PAGES
                if type(value['page']) is not int or not 0 <= value['page'] < page_limit:
                    raise InvalidRequest('Invalid page.')
                data['page'] = value['page']
            if 'snapshot_id' in value:
                data['snapshot_id'] = stable_id(value['snapshot_id'])
                if 'page' not in value or 'before' in value:
                    raise InvalidRequest('Snapshot page required; before cannot change.')
            elif data.get('page', 0) != 0:
                raise InvalidRequest('First page must be zero.')
            if action == 'load_history':
                query = {'thread_id': [data['thread_id']], 'limit': [str(HISTORY_LIMIT)]}
                if 'before' in value:
                    query['before'] = [_text(value['before'], 20)]
                    data['before'] = value['before']
                history_query(query)
            if action == 'reconnect':
                data['reconnect'] = True
            if action == 'stop_turn':
                data['idempotency_key'] = rid
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


def _metadata(rid, tag, *fields, state='metadata'):
    value = '\t'.join(['ncm1', tag] + [quote(str(field), safe='') for field in fields])
    output = _output(rid, 'ack', value, state)
    output['event_id'] = rid + ':metadata:' + tag
    return output


def _snapshot_limit():
    raise APIError('Snapshot exceeds retained-content limit.', 413)


def _utf8_pages(text, limit):
    """Split complete Unicode text without dropping bytes or splitting a codepoint."""
    encoded, pages = text.encode('utf-8'), []
    if len(encoded) > MAX_SNAPSHOT_BYTES:
        _snapshot_limit()
    while encoded:
        end = min(limit, len(encoded))
        while end < len(encoded) and encoded[end] & 0xc0 == 0x80:
            end -= 1
        pages.append(encoded[:end].decode('utf-8'))
        encoded = encoded[end:]
        if len(pages) > MAX_PAGES:
            _snapshot_limit()
    return pages or ['']


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
                    record['outputs'] = (self._project_page(rid, data, record)
                                         if 'page' in data else [self._projects(rid)])
                elif route == '/api/messages':
                    record['outputs'] = [self._history_page(rid, data, record)]
                elif route == '/api/status':
                    if data.get('reconnect'):
                        resume = getattr(self.backend, 'resume', None)
                        if callable(resume):
                            resume()  # Supported durable subscription restoration, no new turns.
                    status = self.status()
                    record['outputs'] = [_metadata(rid, 'connection', status['state'], state='completed')]
                elif route == '/api/turns/cancel':
                    result = self.backend.handle('POST', route, {}, data)
                    receipt = result.get('receipt', {})
                    if (result.get('thread_id') != data['thread_id'] or result.get('turn_id') != data['turn_id']
                            or receipt.get('turn_id') != data['turn_id']
                            or receipt.get('state') not in ('cancelling', 'cancelled', 'completed', 'failed')):
                        raise InvalidRequest('Unconfirmed cancellation receipt.')
                    record['outputs'] = [
                        _output(rid, 'ack', 'Stop requested; completion is not yet confirmed.', 'completed'),
                        _metadata(rid, 'stop', data['thread_id'], data['turn_id'], 'requested')]
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
                    if route == '/api/send':
                        record['outputs'].append(_metadata(rid, 'send', rid, thread, turn, state))
                    elif route.endswith('/create'):
                        record['outputs'].append(_metadata(rid, 'created', rid,
                            _receipt_id(result.get('project_id')), _receipt_id(result.get('thread_id')),
                            data[name_field]))
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

    def _snapshot(self, data, kind):
        saved = self._read(data['snapshot_id'])
        snapshot = saved and saved[1].get('snapshot')
        if not snapshot or snapshot.get('kind') != kind:
            raise APIError('Unknown snapshot.', 400)
        if kind == 'history' and (snapshot['thread'] != data['thread_id'] or snapshot['view'] != data['view_id']):
            raise APIError('Snapshot belongs to another view.', 400)
        if data['page'] >= len(snapshot['pages']):
            raise APIError('Page outside snapshot.', 400)
        return snapshot

    def _project_page(self, rid, data, record):
        if 'snapshot_id' in data:
            snapshot = self._snapshot(data, 'projects')
            snapshot_id = data['snapshot_id']
        else:
            workspace = self.backend.handle('GET', '/api/workspace', {}, {})
            projects, threads = workspace['projects'], workspace['threads']
            if (not isinstance(projects, list) or not isinstance(threads, list)
                    or len(projects) + len(threads) > MAX_ROWS):
                _snapshot_limit()
            by_project, seen_threads = {}, set()
            for project_row in projects:
                pid = identifier(project_row['id'])
                if pid in by_project:
                    raise InvalidRequest('Duplicate project.')
                by_project[pid] = []
            for thread in threads:
                pid, tid = identifier(thread['project_id']), identifier(thread['id'])
                if pid not in by_project or tid in seen_threads or type(thread.get('closed', False)) is not bool:
                    raise InvalidRequest('Invalid workspace thread.')
                seen_threads.add(tid)
                by_project[pid].append(thread)
            pages, rows = [], ['ncw1']
            page_bytes, total_bytes, count = 4, 4, 0
            def add(fields):
                nonlocal rows, page_bytes, total_bytes, count
                row = fields[0] + '\t' + '\t'.join(quote(_text(s, 4096, controls=True), safe='') for s in fields[1:])
                size = 1 + len(row.encode('utf-8'))
                count += 1
                total_bytes += size
                if count > MAX_ROWS or total_bytes > MAX_CATALOG_BYTES or 4 + size > MAX_VALUE:
                    _snapshot_limit()
                if page_bytes + size > MAX_VALUE:
                    total_bytes += 4  # Each retained page repeats the ncw1 header.
                    if total_bytes > MAX_CATALOG_BYTES:
                        _snapshot_limit()
                    pages.append('\n'.join(rows))
                    rows, page_bytes = ['ncw1'], 4
                rows.append(row)
                page_bytes += size
            for project_row in projects:
                pid = identifier(project_row['id'])
                add(['P', pid, project_row['name']])
                for thread in by_project[pid]:
                    add(['T', pid, thread['id'], thread['title'],
                         'closed' if thread.get('closed', False) else thread.get('status', 'unknown')])
            pages.append('\n'.join(rows))
            if len(pages) > MAX_CATALOG_PAGES:
                _snapshot_limit()
            snapshot = {'kind': 'projects', 'pages': pages}
            record['snapshot'] = snapshot
            snapshot_id = rid
        page = data.get('page', 0)
        return [_metadata(rid, 'projects', snapshot_id, page, len(snapshot['pages'])),
                _output(rid, 'projects', snapshot['pages'][page], 'completed')]

    def _history_page(self, rid, data, record):
        if 'snapshot_id' in data:
            snapshot = self._snapshot(data, 'history')
            snapshot_id = data['snapshot_id']
        else:
            query = {'thread_id': [data['thread_id']], 'limit': [str(HISTORY_LIMIT)]}
            if 'before' in data:
                query['before'] = [data['before']]
            result = self.backend.handle('GET', '/api/messages', query, {})
            details = result['message_details']
            if not isinstance(details, list) or len(details) > HISTORY_LIMIT or type(result.get('has_more')) is not bool:
                raise InvalidRequest('Invalid history.')
            before = result.get('first_cursor')
            if before is not None:
                history_query({'thread_id': [data['thread_id']], 'before': [before]})
            more = result['has_more']
            if more and (before is None or ('before' in data and int(before) >= int(data['before']))):
                raise InvalidRequest('History cursor did not advance.')
            texts, size = [], 0
            for detail in details:
                role = detail.get('role')
                if role not in ('user', 'assistant'):
                    raise InvalidRequest('Invalid history role.')
                text = ('You' if role == 'user' else 'Assistant') + ':\n' + _text(detail.get('text'), MAX_SNAPSHOT_BYTES)
                size += len(text.encode('utf-8')) + (2 if texts else 0)
                if size > MAX_SNAPSHOT_BYTES:
                    _snapshot_limit()
                texts.append(text)
            snapshot = {'kind': 'history', 'thread': data['thread_id'], 'view': data['view_id'],
                        'before': before or '', 'more': more}
            # Budget worst-case page number/count; all identifiers are bounded.
            header = self._history_header(rid, snapshot, MAX_PAGES - 1, MAX_PAGES)
            snapshot['pages'] = _utf8_pages('\n\n'.join(texts), MAX_VALUE - len(header.encode('utf-8')))
            record['snapshot'] = snapshot
            snapshot_id = rid
        page = data.get('page', 0)
        header = self._history_header(snapshot_id, snapshot, page, len(snapshot['pages']))
        return _output(rid, 'reply', header + snapshot['pages'][page], 'completed')

    @staticmethod
    def _history_header(snapshot_id, snapshot, page, pages):
        fields = [snapshot_id, snapshot['thread'], str(page), str(pages), snapshot['before'],
                  '1' if snapshot['more'] else '0', snapshot['view']]
        return 'nch1\t' + '\t'.join(quote(value, safe='') for value in fields) + '\n'

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
            record['outputs'].append(_metadata(rid, 'reply', rid, record['thread'], record['turn']))
            record['outputs'].append(_output(rid, 'reply', text, 'reply'))
            record['done'] = True
        elif kind in ('turn_failed', 'turn_cancelled'):
            record['outputs'].append(_output(rid, 'error', 'Turn failed or was cancelled.', 'error'))
            record['done'] = True
