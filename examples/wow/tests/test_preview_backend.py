"""Local HTTP and durable projection tests; no real credentials or model requests."""
from concurrent.futures import ThreadPoolExecutor
import http.client
import importlib.util
import json
from pathlib import Path
import tempfile
import threading
import unittest
from unittest.mock import patch

from durable_client import DurableBackend, EventStore
from transport.streaming import decode

SPEC = importlib.util.spec_from_file_location(
    'preview_backend', Path(__file__).resolve().parents[1] / 'preview/backend-server.py')
preview = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(preview)


class Backend(DurableBackend):
    def __init__(self, directory):
        super().__init__(directory)
        self.sends = 0
        self.opens = 0
        self.status_checks = 0
        self.subscriptions = []

    def _open_store(self):
        self.opens += 1
        if self.store is None:
            self.fingerprint = 'synthetic-test-account'
            self.store = EventStore(self.storage_dir / 'events.sqlite3')

    def subscribe(self, thread):
        self._open_store()
        self.clients[thread] = None
        self.subscriptions.append(thread)

    def handle(self, method, path, query, data):
        if path == '/api/status':
            self.status_checks += 1
            return {'connected': True, 'error': 'MUST_NOT_LEAK', 'account': 'private'}
        if path == '/api/send':
            self.sends += 1
            self.subscribe(data['thread_id'])
            return {'thread_id': data['thread_id'], 'turn_id': data['idempotency_key'], 'status': 'queued'}
        raise AssertionError('Unexpected backend call: ' + path)

    def close(self):
        if self.store is not None:
            self.store.close()
            self.store = None


def payload(prompt='Hello'):
    return json.dumps({'schemaVersion': 1, 'source': 'nanocodex-wow',
                      'type': 'nanocodex.ask', 'prompt': prompt, 'mode': 'agent',
                      'thread_id': 'thread1'})


class PreviewTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.public = self.root / 'public'
        self.public.mkdir()
        (self.public / 'index.html').write_text('<title>Actual addon</title>')
        (self.root / 'private.txt').write_text('MUST_NOT_LEAK')
        (self.public / 'escape.txt').symlink_to(self.root / 'private.txt')
        self.backend = Backend(self.root / 'private')
        self.start()
        self.addCleanup(self.stop)

    def start(self):
        self.server = preview.make_server(0, self.backend, self.public)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        self.port = self.server.server_address[1]

    def stop(self):
        if self.server is not None:
            self.server.shutdown()
            self.server.server_close()
            self.thread.join()
            self.server = None

    def request(self, method, path, data=None, headers=None):
        client = http.client.HTTPConnection('127.0.0.1', self.port, timeout=3)
        hdr = {'Content-Type': 'application/json'}
        if method == 'POST':
            hdr['Origin'] = f'http://127.0.0.1:{self.port}'
        for key, value in (headers or {}).items():
            if value is None:
                hdr.pop(key, None)
            else:
                hdr[key] = value
        body = json.dumps(data) if data is not None else None
        client.request(method, path, body, hdr)
        response = client.getresponse()
        raw = response.read()
        status, response_headers = response.status, dict(response.getheaders())
        client.close()
        return status, raw, response_headers

    def dispatch(self, rid='ask1', prompt='Hello'):
        status, raw, _ = self.request('POST', '/api/addon/dispatch', {'payload': payload(prompt), 'id': rid})
        self.assertEqual(status, 200, raw)
        return json.loads(raw)['outputs']

    def poll(self, query=''):
        status, raw, _ = self.request('GET', '/api/addon/poll' + query)
        self.assertEqual(status, 200, raw)
        return json.loads(raw)['outputs']

    def event(self, cursor, kind, **fields):
        self.backend.store.accept('thread1', {'cursor': str(cursor), 'type': kind, **fields})

    def test_static_status_and_startup_have_no_subscriptions(self):
        status, body, _ = self.request('GET', '/')
        self.assertEqual((status, body), (200, b'<title>Actual addon</title>'))
        status, body, headers = self.request('GET', '/api/addon/status')
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(body), {'connected': True})
        self.assertNotIn('Access-Control-Allow-Origin', headers)
        self.assertEqual((self.backend.sends, self.backend.opens, self.backend.subscriptions), (0, 0, []))

    def test_host_origin_and_cross_site_rejected_before_backend_access(self):
        cases = [{'Host': 'attacker.example'}, {'Origin': 'https://attacker.example'},
                 {'Origin': None}, {'Origin': 'null'}, {'Sec-Fetch-Site': 'cross-site'}]
        for headers in cases:
            with self.subTest(headers=headers):
                status, _, _ = self.request('POST', '/api/addon/dispatch',
                                           {'payload': payload(), 'id': 'ask1'}, headers)
                self.assertEqual(status, 403)
        self.assertEqual(self.backend.opens, 0)
        self.assertEqual(self.backend.sends, 0)
        self.assertEqual(self.request('OPTIONS', '/api/addon/dispatch')[0], 403)
        self.assertEqual(self.request('GET', '/api/addon/status', headers={'Host': 'attacker.example'})[0], 403)

    def test_static_traversal_and_symlinks_are_rejected(self):
        for path in ('/../private.txt', '/%2e%2e/private.txt', '/escape.txt', '/api/send'):
            self.assertEqual(self.request('GET', path)[0], 404)

    def test_invalid_envelopes_do_not_open_auth_store(self):
        for data in ({}, {'payload': {}, 'id': 'x'}, {'payload': payload(), 'id': 'bad/id'},
                     {'payload': payload(), 'id': 'x', 'token': 'FORBIDDEN'}):
            self.assertEqual(self.request('POST', '/api/addon/dispatch', data)[0], 400)
        self.assertEqual(self.backend.opens, 0)
        self.assertEqual(self.backend.sends, 0)

    def test_incremental_events_and_duplicate_request_replay(self):
        queued = self.dispatch()
        self.assertEqual(queued[0]['state'], 'local_queued')
        self.assertEqual(self.dispatch(), queued)
        self.assertEqual(self.backend.sends, 1)
        self.event(1, 'turn_accepted', id='ask1')
        self.event(2, 'event', turn_id='ask1', event={'type': 'assistant.delta', 'payload': {
            'text': 'Hello ', 'item_id': 'reply1', 'phase': 'final_answer'}})
        first = self.poll()
        stream = [decode(item['value']) for item in first if item['kind'] == 'stream']
        self.assertEqual([item['text'] for item in stream], ['Hello '])
        self.assertEqual(first, self.poll())
        self.event(3, 'event', turn_id='ask1', event={'type': 'assistant.delta', 'payload': {
            'text': 'WoW', 'item_id': 'reply1', 'phase': 'final_answer'}})
        second = self.poll()
        self.assertEqual(second[:len(first)], first)
        self.assertEqual(decode(second[-1]['value'])['text'], 'WoW')
        self.event(4, 'turn_completed', id='ask1', final_message='Hello WoW')
        completed = self.poll()
        self.assertEqual(decode(completed[-1]['value'])['op'], 'done')
        self.assertEqual(len({row['event_id'] for row in completed}), len(completed))
        self.assertEqual(completed, self.poll('?id=ask1'))
        self.assertEqual(self.backend.sends, 1)

    def test_slow_roster_and_history_do_not_block_other_thread_sends_or_streams(self):
        original = self.backend.handle
        for action, route, fields in (
                ('refresh_projects', '/api/workspace', {'page': 0}),
                ('load_history', '/api/messages', {'thread_id': 'thread1', 'view_id': 'view1'})):
            with self.subTest(action=action):
                entered, release = threading.Event(), threading.Event()
                calls = []
                rid, fast_id = action, action + '-send'
                request_payload = json.dumps({'schemaVersion': 1, 'source': 'nanocodex-wow',
                                              'type': 'nanocodex.action', 'action': action, **fields})

                def blocked(method, path, query, data):
                    if path == route:
                        calls.append(path)
                        entered.set()
                        if not release.wait(5):
                            raise TimeoutError('Test release timed out')
                        if path == '/api/workspace':
                            return {'projects': [{'id': 'p', 'name': 'Project'}], 'threads': []}
                        return {'message_details': [{'role': 'assistant', 'text': 'History'}],
                                'has_more': False, 'first_cursor': '1'}
                    return original(method, path, query, data)

                with patch.object(self.backend, 'handle', side_effect=blocked), ThreadPoolExecutor(4) as pool:
                    slow = pool.submit(self.request, 'POST', '/api/addon/dispatch',
                                       {'payload': request_payload, 'id': rid})
                    try:
                        self.assertTrue(entered.wait(2))
                        duplicate = pool.submit(self.request, 'POST', '/api/addon/dispatch',
                                                {'payload': request_payload, 'id': rid}).result(2)
                        self.assertEqual((duplicate[0], json.loads(duplicate[1])), (200, {'outputs': []}))
                        self.assertEqual(pool.submit(self.poll, '?id=' + rid).result(2), [])
                        fast_payload = json.loads(payload())
                        fast_payload['thread_id'] = 'thread2'
                        fast = pool.submit(self.request, 'POST', '/api/addon/dispatch',
                                           {'payload': json.dumps(fast_payload), 'id': fast_id}).result(2)
                        self.assertEqual(fast[0], 200)
                        self.assertEqual(json.loads(fast[1])['outputs'][0]['state'], 'local_queued')
                        position = int(self.backend.store.position('thread2'))
                        self.backend.store.accept('thread2', {'cursor': str(position + 1),
                            'type': 'turn_completed', 'id': fast_id, 'final_message': 'Independent reply'})
                        polled = pool.submit(self.poll).result(2)
                        stream = [decode(row['value']) for row in polled
                                  if row['request_id'] == fast_id and row['kind'] == 'stream']
                        self.assertEqual(stream[-1]['op'], 'done')
                        self.assertIn('Independent reply', ''.join(row['text'] for row in stream))
                        self.assertFalse(slow.done())
                        self.assertEqual(calls, [route])
                    finally:
                        release.set()
                    status, body, _ = slow.result(2)
                self.assertEqual(status, 200)
                completed = json.loads(body)['outputs']
                self.assertEqual(completed[-1]['kind'], 'projects' if action == 'refresh_projects' else 'reply')
                self.assertEqual(self.poll('?id=' + rid), completed)
                self.assertEqual(calls, [route])
        self.assertEqual(self.backend.sends, 2)

    def test_account_change_fences_cached_receipts_snapshots_and_polls(self):
        queued = self.dispatch()
        with patch.object(self.backend, 'handle', return_value={'projects': [], 'threads': []}):
            roster = json.dumps({'schemaVersion': 1, 'source': 'nanocodex-wow',
                                 'type': 'nanocodex.action', 'action': 'refresh_projects', 'page': 0})
            self.assertEqual(self.request('POST', '/api/addon/dispatch', {'payload': roster, 'id': 'roster'})[0], 200)
        with patch.object(self.backend, '_open_store', side_effect=preview.APIError('Account changed.', 401)):
            self.assertEqual(self.request('GET', '/api/addon/poll?id=ask1')[0], 401)
            self.assertEqual(self.request('POST', '/api/addon/dispatch', {'payload': payload(), 'id': 'ask1'})[0], 401)
            continuation = json.loads(roster)
            continuation['snapshot_id'] = 'roster'
            self.assertEqual(self.request('POST', '/api/addon/dispatch',
                                         {'payload': json.dumps(continuation), 'id': 'page1'})[0], 401)
        self.assertEqual(self.poll('?id=ask1'), queued)
        self.assertEqual(self.backend.sends, 1)

    def test_journal_restart_keeps_identity_and_restores_active_request(self):
        queued = self.dispatch()
        self.stop()
        self.backend = Backend(self.root / 'private')
        self.start()
        self.assertEqual(self.backend.sends, 0)
        self.assertEqual(self.poll(), queued)
        self.assertEqual(self.dispatch(), queued)
        self.assertEqual(self.backend.sends, 0)
        conflict = self.dispatch(prompt='Different prompt')
        self.assertEqual(conflict[0]['value'], 'Request ID conflict.')
        self.assertEqual(self.backend.sends, 0)


if __name__ == '__main__':
    unittest.main()
