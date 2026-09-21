"""In-game client actions through the real journal/dispatcher and backend adapters."""
import json
from pathlib import Path
import tempfile
import unittest
from urllib.parse import unquote

from server import APIError, Backend
from transport.dispatch import Dispatcher, MAX_ROWS, MAX_VALUE, MAX_SNAPSHOT_BYTES, encode_output, wire_fragments
from transport.messages import Assembler


def action(action_name, **fields):
    return json.dumps({'schemaVersion': 1, 'source': 'nanocodex-wow',
                      'type': 'nanocodex.action', 'action': action_name, **fields})


class ClientBackend(Backend):
    """Real /api/messages and /api/turns/cancel adapters, synthetic upstream IO."""
    def __init__(self):
        self.calls, self.upstream = [], []
        self.roster = [{'id': 'p', 'name': 'Project Ω'}]
        self.threads = {'p': [{'id': 't', 'title': 'Chat', 'status': 'unknown'}]}
        self.closed = {'p': []}
        self.events = {'data': [], 'has_more': False}
        self.cancel = {'turn_id': 'turn:1', 'state': 'cancelling'}
        self.connected, self.resumes, self.failure = True, 0, None

    def request(self, method, path, payload=None, key=None):
        self.upstream.append((method, path, payload, key))
        if '/events/history?' in path:
            return self.events
        if path.endswith('/cancel'):
            return self.cancel
        raise AssertionError('Unexpected upstream route: ' + path)

    def resume(self):
        self.resumes += 1
        if self.failure:
            raise self.failure

    def handle(self, method, path, query, data):
        self.calls.append((method, path, query, data))
        if self.failure:
            raise self.failure
        if path == '/api/workspace':
            threads = [{**row, 'project_id': pid, 'closed': closed}
                       for closed, groups in ((False, self.threads), (True, self.closed))
                       for pid, rows in groups.items() for row in rows]
            return {'projects': self.roster, 'threads': threads}
        if path == '/api/projects':
            return {'projects': self.roster}
        if path == '/api/threads':
            rows = self.closed if query.get('closed') == ['true'] else self.threads
            return {'threads': rows.get(query['project_id'][0], [])}
        if path == '/api/status':
            return {'connected': self.connected, 'error': 'PRIVATE', 'base_url': 'PRIVATE'}
        if path == '/api/send':
            return {'thread_id': data.get('thread_id', 't'), 'turn_id': data['idempotency_key'], 'status': 'queued'}
        if path == '/api/threads/create':
            return {'project_id': data.get('project_id', 'created'), 'thread_id': 'created',
                    'title': data['title'], 'status': 'created'}
        return super().handle(method, path, query, data)


class ClientDispatchTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.path = Path(temp.name) / 'dispatch.db'
        self.backend = ClientBackend()
        self.d = Dispatcher(self.backend, self.path)
        self.addCleanup(lambda: self.d.close())

    def dispatch(self, action_name, rid='request', **fields):
        return self.d.dispatch(action(action_name, **fields), rid)

    def restart(self):
        self.d.close()
        self.d = Dispatcher(self.backend, self.path)

    def roundtrip(self, outputs):
        received = []
        assembler = Assembler(lambda kind, body: received.append((kind, body.decode())),
                              accepted_kinds=('A', 'R', 'P', 'E'))
        for message, output in enumerate(outputs, 1):
            self.assertLessEqual(len(encode_output(output)), MAX_VALUE)
            for fragment in wire_fragments(output, message):
                self.assertTrue(assembler.receive(fragment))
        self.assertEqual(received, [(o['kind'], o['value']) for o in outputs])

    def test_project_pages_reassemble_all_open_and_closed_without_repeat_io(self):
        self.backend.threads['p'] = [
            {'id': 'thread' + str(i), 'title': ('Ω 🎮 ' * 30) + str(i)} for i in range(100)]
        self.backend.closed['p'] = [{'id': 'closed', 'title': 'Closed chat'}]
        first = self.dispatch('refresh_projects', 'roster:1', page=0)
        self.assertEqual([o['kind'] for o in first], ['ack', 'projects'])
        header = first[0]['value'].split('\t')
        self.assertEqual(header[:4], ['ncm1', 'projects', 'roster%3A1', '0'])
        count = int(header[4])
        self.assertGreater(count, 1)
        self.assertEqual(self.backend.calls, [('GET', '/api/workspace', {}, {})])
        self.restart()
        self.backend.failure = AssertionError('Snapshot must not refetch live roster')
        pages, outputs = [first[1]['value']], first[:]
        for page in range(1, count):
            result = self.dispatch('refresh_projects', f'page:{page}', snapshot_id='roster:1', page=page)
            pages.append(result[1]['value'])
            outputs.extend(result)
            self.assertEqual(self.dispatch('refresh_projects', f'page:{page}', snapshot_id='roster:1', page=page), result)
        combined = pages[0] + ''.join(p[4:] for p in pages[1:])
        rows = [list(map(unquote, line.split('\t'))) for line in combined.splitlines()[1:]]
        self.assertEqual(len(rows), 102)
        self.assertEqual(len({row[2] for row in rows if row[0] == 'T'}), 101)
        self.assertEqual(rows[-1], ['T', 'p', 'closed', 'Closed chat', 'closed'])
        self.assertEqual(len({o['event_id'] for o in outputs}), len(outputs))
        self.assertEqual(self.dispatch('refresh_projects', 'roster:1', page=0), first)
        self.roundtrip(outputs)

    def test_project_limits_and_duplicates_emit_only_error(self):
        cases = [([{'id': 't' + str(i), 'title': 'T'} for i in range(MAX_ROWS)], []),
                 ([{'id': 't', 'title': 'T'}], [{'id': 't', 'title': 'T'}]),
                 ([{'id': 't', 'title': 'bad\ntitle'}], []),
                 ([{'id': 't' + str(i), 'title': 'Ω' * 512} for i in range(400)], [])]
        for i, (rows, closed) in enumerate(cases):
            self.backend.threads['p'], self.backend.closed['p'] = rows, closed
            result = self.dispatch('refresh_projects', f'limit{i}', page=0)
            self.assertEqual([o['kind'] for o in result], ['error'])
            self.assertNotIn('snapshot', self.d._read(f'limit{i}')[1])

    def test_invalid_page_references_do_not_refetch(self):
        self.dispatch('refresh_projects', 'roster', page=0)
        calls = len(self.backend.calls)
        for i, fields in enumerate(({'snapshot_id': 'missing', 'page': 0},
                                    {'snapshot_id': 'roster', 'page': 1}, {'page': 1},
                                    {'page': True}, {'page': -1}, {'page': 128},
                                    {'snapshot_id': 'roster'})):
            self.assertEqual(self.dispatch('refresh_projects', f'invalid{i}', **fields)[0]['state'], 'error')
        self.assertEqual(len(self.backend.calls), calls)

    def test_history_utf8_pages_exact_bytes_and_view_binding(self):
        answer = 'Ω🎮\n|Hitem:123|hNot markup|h' * 2400
        self.backend.events = {'data': [
            {'type': 'event', 'cursor': '100', 'event': {'type': 'tool.result'}},
            {'type': 'turn_accepted', 'id': 'turn:1', 'cursor': '101', 'input': 'Question'},
            {'type': 'turn_completed', 'id': 'turn:1', 'cursor': '102', 'final_message': answer}], 'has_more': True}
        first = self.dispatch('load_history', 'history:1', thread_id='t', view_id='view:1')
        header, body = first[0]['value'].split('\n', 1)
        fields = list(map(unquote, header.split('\t')))
        self.assertEqual(fields[:4], ['nch1', 'history:1', 't', '0'])
        self.assertEqual(fields[5:], ['100', '1', 'view:1'])
        self.assertEqual(self.backend.upstream, [('GET', '/v1/agents/t/events/history?limit=64', None, None)])
        self.restart()
        self.backend.failure = AssertionError('No history refetch for retained pages')
        outputs, parts = first[:], [body]
        for page in range(1, int(fields[4])):
            result = self.dispatch('load_history', f'history-page:{page}', thread_id='t', view_id='view:1',
                                   snapshot_id='history:1', page=page)
            outputs.extend(result)
            parts.append(result[0]['value'].split('\n', 1)[1])
        self.assertEqual(''.join(parts), 'You:\nQuestion\n\nAssistant:\n' + answer)
        for i, changes in enumerate(({'view_id': 'view:2'}, {'thread_id': 'different'})):
            request = {'thread_id': 't', 'view_id': 'view:1', 'snapshot_id': 'history:1', 'page': 0, **changes}
            self.assertEqual(self.dispatch('load_history', f'stale{i}', **request)[0]['state'], 'error')
        self.assertEqual(self.dispatch('refresh_projects', 'wrong-kind', snapshot_id='history:1', page=0)[0]['state'], 'error')
        self.roundtrip(outputs)

    def test_older_history_uses_raw_event_cursor_including_empty_visible_batch(self):
        self.backend.events = {'data': [{'type': 'event', 'cursor': '41', 'event': {'type': 'tool.result'}}], 'has_more': True}
        result = self.dispatch('load_history', thread_id='t', view_id='view', before='50')[0]
        self.assertEqual(result['value'], 'nch1\trequest\tt\t0\t1\t41\t1\tview\n')
        self.assertEqual(self.backend.upstream[-1][1], '/v1/agents/t/events/history?limit=64&before=50')

    def test_history_rejects_nonadvancing_cursor_and_oversize_without_partial(self):
        for i, events in enumerate((
                {'data': [], 'has_more': True},
                {'data': [{'type': 'event', 'cursor': '50'}], 'has_more': True},
                {'data': [{'type': 'turn_completed', 'cursor': '40', 'id': 't',
                           'final_message': 'x' * MAX_SNAPSHOT_BYTES}], 'has_more': False})):
            self.backend.events = events
            result = self.dispatch('load_history', f'bad-history{i}', thread_id='t', view_id='v', before='50')
            self.assertEqual([o['kind'] for o in result], ['error'])

    def test_new_fields_are_strict_and_never_forward_credentials_or_routes(self):
        invalid = [('load_history', {'thread_id': 't'}),
                   ('load_history', {'thread_id': 't', 'view_id': 'v', 'before': 1}),
                   ('load_history', {'thread_id': 't', 'view_id': 'v', 'page': 32}),
                   ('load_history', {'thread_id': 't', 'view_id': 'v', 'before': '0'}),
                   ('load_history', {'thread_id': 't', 'view_id': 'v', 'before': '9223372036854775808'}),
                   ('load_history', {'thread_id': 't', 'view_id': 'v', 'before': '5', 'snapshot_id': 's', 'page': 0}),
                   ('stop_turn', {'thread_id': 't', 'turn_id': '../bad'}),
                   ('stop_turn', {'thread_id': 't'}),
                   ('reconnect', {'token': 'PRIVATE'}), ('connection_status', {'path': '/api/connect'})]
        for i, (name, fields) in enumerate(invalid):
            self.assertEqual(self.dispatch(name, f'bad{i}', **fields)[0]['state'], 'error')
        self.assertEqual(self.backend.calls, [])

    def test_stop_exact_backend_route_and_restart_dedup(self):
        result = self.dispatch('stop_turn', 'cancel:1', thread_id='t', turn_id='turn:1')
        self.assertEqual(result[-1]['value'], 'ncm1\tstop\tt\tturn%3A1\trequested')
        self.assertIn('not yet confirmed', result[0]['value'])
        self.assertEqual(self.backend.calls[-1], ('POST', '/api/turns/cancel', {},
                         {'thread_id': 't', 'turn_id': 'turn:1', 'idempotency_key': 'cancel:1'}))
        self.assertEqual(self.backend.upstream, [('POST', '/v1/agents/t/turns/turn%3A1/cancel', None, None)])
        self.restart()
        self.assertEqual(self.dispatch('stop_turn', 'cancel:1', thread_id='t', turn_id='turn:1'), result)
        self.assertEqual(self.d.poll('cancel:1'), result)
        self.assertEqual(len(self.backend.upstream), 1)

    def test_stop_unknown_or_malformed_never_claims_success_or_retries(self):
        for i, receipt in enumerate(({}, {'turn_id': 'other', 'state': 'cancelling'},
                                     {'turn_id': 'turn:1', 'state': 'accepted'})):
            self.backend.cancel = receipt
            result = self.dispatch('stop_turn', f'malformed{i}', thread_id='t', turn_id='turn:1')
            self.assertEqual([o['state'] for o in result], ['unknown'])
        self.backend.failure = TimeoutError('PRIVATE')
        result = self.dispatch('stop_turn', 'uncertain', thread_id='t', turn_id='turn:1')
        self.assertEqual(result[0]['state'], 'unknown')
        self.assertNotIn('PRIVATE', json.dumps(result))
        count = len(self.backend.calls)
        self.restart()
        self.backend.failure = None
        self.assertEqual(self.dispatch('stop_turn', 'uncertain', thread_id='t', turn_id='turn:1'), result)
        self.assertEqual(len(self.backend.calls), count)

    def test_connection_recheck_resume_and_safe_failures(self):
        status = self.dispatch('connection_status', 'status')[0]
        self.assertEqual(status['value'], 'ncm1\tconnection\tconnected')
        self.assertEqual(self.backend.resumes, 0)
        self.backend.connected = False
        result = self.dispatch('reconnect', 'reconnect')
        self.assertEqual(result[0]['value'], 'ncm1\tconnection\tdisconnected')
        self.assertEqual(self.backend.resumes, 1)
        self.assertEqual(self.dispatch('reconnect', 'reconnect'), result)
        self.assertEqual(self.backend.resumes, 1)
        self.assertTrue(all(call[:2] == ('GET', '/api/status') for call in self.backend.calls))
        self.backend.failure = APIError('PRIVATE', 401)
        self.assertEqual(self.dispatch('connection_status', 'offline')[0]['value'], 'ncm1\tconnection\tdisconnected')
        self.assertNotIn('PRIVATE', json.dumps(self.dispatch('reconnect', 'failed-resume')))

    def test_fallback_reply_identity_precedes_each_plain_answer_and_replays_after_restart(self):
        completed = {}
        for thread, rid in (('first-thread', 'first:turn'), ('second-thread', 'second:turn')):
            request = json.dumps({'schemaVersion': 1, 'source': 'nanocodex-wow', 'type': 'nanocodex.ask',
                                  'mode': 'agent', 'prompt': 'Hello', 'thread_id': thread})
            self.d.dispatch(request, rid)
        # Complete in reverse order: each routing receipt identifies its own answer.
        for thread, rid in (('second-thread', 'second:turn'), ('first-thread', 'first:turn')):
            self.backend.events = {'data': [{'type': 'turn_completed', 'id': rid, 'cursor': '1',
                                            'final_message': 'Answer for ' + thread}], 'has_more': False}
            result = self.d.poll(rid)
            metadata, answer = result[-2:]
            self.assertEqual(metadata['kind'], 'ack')
            self.assertEqual(metadata['state'], 'metadata')
            self.assertEqual(metadata['event_id'], rid + ':metadata:reply')
            self.assertEqual(list(map(unquote, metadata['value'].split('\t'))),
                             ['ncm1', 'reply', rid, thread, rid])
            self.assertEqual((answer['kind'], answer['state'], answer['value']),
                             ('reply', 'reply', 'Answer for ' + thread))
            self.assertEqual(len({o['event_id'] for o in result}), len(result))
            self.roundtrip(result)
            completed[rid] = result
        self.restart()
        calls = len(self.backend.calls)
        self.backend.failure = AssertionError('Completed fallback answers must not refetch')
        for rid, result in completed.items():
            self.assertEqual(self.d.poll(rid), result)
        self.assertEqual(len(self.backend.calls), calls)

    def test_send_metadata_preserves_nonterminal_poll_and_new_chat_selection(self):
        request = json.dumps({'schemaVersion': 1, 'source': 'nanocodex-wow', 'type': 'nanocodex.ask',
                              'mode': 'agent', 'prompt': 'Hello', 'thread_id': 't'})
        result = self.d.dispatch(request, 'send:1')
        self.assertEqual([o['state'] for o in result], ['local_queued', 'metadata'])
        self.assertEqual(result[1]['value'], 'ncm1\tsend\tsend%3A1\tt\tsend%3A1\tlocal_queued')
        self.backend.events = {'data': [{'type': 'turn_completed', 'id': 'send:1', 'cursor': '1',
                                        'final_message': 'Done'}], 'has_more': False}
        self.assertEqual(self.d.poll('send:1')[-1]['value'], 'Done')
        created = self.dispatch('create_chat', 'create:1', name='New Ω')
        self.assertEqual(created[-1]['value'], 'ncm1\tcreated\tcreate%3A1\tcreated\tcreated\tNew%20%CE%A9')
        self.assertEqual(self.backend.calls[-1], ('POST', '/api/threads/create', {},
                         {'title': 'New Ω', 'idempotency_key': 'create:1'}))


if __name__ == '__main__':
    unittest.main()
