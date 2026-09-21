"""Contract tests grounded in the checked Apple / Cloudflare API sources."""
import http.client
import json
import os
from pathlib import Path
import tempfile
import threading
import unittest
from unittest.mock import patch
import server


class RecordingBackend(server.Backend):
    def __init__(self):
        self.calls = []
        self.base_url = 'https://example.invalid'
        self.response = {}
    def request(self, method, path, payload=None, key=None):
        self.calls.append((method, path, payload, key))
        return self.response


class ManagementTests(unittest.TestCase):
    def setUp(self):
        self.backend = RecordingBackend()
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        home = patch.object(server.Path, 'home', return_value=Path(temporary.name))
        home.start(); self.addCleanup(home.stop)
        credentials = patch.object(RecordingBackend, 'credentials', return_value='test-account')
        credentials.start(); self.addCleanup(credentials.stop)

    def call(self, path, data=None, query=None, method='POST'):
        return self.backend.handle(method, path, query or {}, data)

    def test_empty_luna_chat_creation(self):
        self.backend.response = {'agent_id': 'new-chat'}
        result = self.call('/api/threads/create', {'idempotency_key': 'create:1'})
        self.assertEqual(result['thread_id'], 'new-chat')
        self.assertEqual(result['metadata_scope'], 'local_companion')
        self.assertEqual(self.backend.calls, [('GET', '/v1/agents', None, None), ('POST', '/v1/agents', {'settings': server.LUNA_SETTINGS}, 'create:1')])

    def test_creation_cannot_claim_missing_receipt(self):
        with self.assertRaises(server.APIError) as error:
            self.call('/api/threads/create', {'idempotency_key': 'create:1'})
        self.assertEqual(error.exception.status, 502)
        self.assertEqual(len(self.backend.calls), 2)

    def test_unsupported_features_never_mutate(self):
        for path in server.UNSUPPORTED_ROUTES:
            with self.subTest(path=path), self.assertRaises(server.APIError) as error:
                self.call(path, {'project_id': 'root', 'name': 'new'})
            self.assertEqual(error.exception.status, 501)
        capabilities = self.call('/api/capabilities', method='GET')['capabilities']
        self.assertFalse(capabilities['child_chat_create'])
        self.assertFalse(capabilities['chat_archive'])
        self.assertTrue(capabilities['standalone_chat_create'])
        self.assertEqual(self.backend.calls, [])

    def test_cancel_uses_bodyless_post_and_real_receipt(self):
        self.backend.response = {'turn_id': 'project-followup:1', 'state': 'cancelling'}
        result = self.call('/api/turns/cancel', {'thread_id': 'root', 'turn_id': 'project-followup:1'})
        self.assertEqual(self.backend.calls, [('POST', '/v1/agents/root/turns/project-followup%3A1/cancel', None, None)])
        self.assertEqual(result['receipt']['state'], 'cancelling')

    def test_steer_uses_message_id_not_turn_admission(self):
        self.call('/api/turns/steer', {'thread_id': 'root', 'turn_id': 't:1', 'text': 'Change direction', 'message_id': 'm:1'})
        self.assertEqual(self.backend.calls, [('POST', '/v1/agents/root/turns/t%3A1/steer', {'input': 'Change direction', 'message_id': 'm:1'}, None)])

    def test_parent_turn_control_keys(self):
        self.call('/api/turns/steer', {'thread_id': 'root', 'turn_id': 't', 'text': 'go', 'idempotency_key': 'm:2'})
        self.assertEqual(self.backend.calls[-1], ('POST', '/v1/agents/root/turns/t/steer', {'input': 'go', 'message_id': 'm:2'}, None))
        result = self.call('/api/turns/cancel', {'thread_id': 'root', 'turn_id': 't', 'idempotency_key': 'cancel:1'})
        self.assertEqual(result['idempotency_key'], 'cancel:1')
        self.assertEqual(self.backend.calls[-1], ('POST', '/v1/agents/root/turns/t/cancel', None, None))

    def test_foreign_organization_ids_rejected(self):
        for path, data in (
            ('/api/projects/update', {'project_id': 'foreign', 'name': 'New'}),
            ('/api/threads/create', {'project_id': 'foreign', 'title': 'Child', 'idempotency_key': 'c'}),
            ('/api/threads/update', {'thread_id': 'foreign', 'title': 'New'}),
            ('/api/threads/close', {'thread_id': 'foreign'}),
            ('/api/threads/restore', {'thread_id': 'foreign'})):
            with self.subTest(path=path), self.assertRaises(server.APIError) as error:
                self.call(path, data)
            self.assertEqual(error.exception.status, 404)
        self.assertTrue(all(call[0] == 'GET' for call in self.backend.calls))

    def test_conflicting_or_invalid_control_keys_never_write(self):
        for path, data in (
            ('/api/turns/steer', {'text': 'go', 'idempotency_key': 'one', 'message_id': 'two'}),
            ('/api/turns/steer', {'text': 'go', 'idempotency_key': ''}),
            ('/api/turns/cancel', {'idempotency_key': '../bad'})):
            with self.subTest(path=path), self.assertRaises(server.APIError):
                self.call(path, {'thread_id': 'root', 'turn_id': 't', **data})
        self.assertEqual(self.backend.calls, [])

    def test_state_reads(self):
        self.backend.response = {'active_turns': [{'id': 't:1'}]}
        result = self.call('/api/thread', query={'thread_id': ['root']}, method='GET')
        self.assertEqual(result['state'], self.backend.response)
        self.call('/api/turn', query={'thread_id': ['root'], 'turn_id': ['t:1']}, method='GET')
        self.assertEqual([c[:2] for c in self.backend.calls], [('GET', '/v1/agents/root'), ('GET', '/v1/agents/root/turns/t%3A1')])

    def test_pagination_uses_raw_event_boundaries(self):
        self.backend.response = {'data': [
            {'cursor': '10', 'type': 'event', 'event': {'type': 'tool.result'}},
            {'cursor': '11', 'type': 'turn_completed', 'id': 't:1', 'final_message': 'Done'},
            {'cursor': '12', 'type': 'event', 'event': {'type': 'tool.result'}}], 'has_more': True, 'latest_cursor': '99'}
        result = self.call('/api/messages', query={'thread_id': ['root'], 'before': ['20'], 'limit': ['3']}, method='GET')
        self.assertEqual(self.backend.calls[0][1], '/v1/agents/root/events/history?limit=3&before=20')
        self.assertEqual((result['first_cursor'], result['last_cursor'], result['latest_cursor']), ('10', '12', '99'))
        self.assertEqual(result['messages'], [{'role': 'assistant', 'text': 'Done'}])
        self.assertEqual(result['message_details'][0]['turn_id'], 't:1')
        self.assertEqual(result['message_details'][0]['cursor'], '11')

    def test_forward_and_empty_visible_pages(self):
        self.backend.response = {'data': [{'cursor': '9223372036854775806', 'type': 'event', 'event': {'type': 'tool.result'}}], 'has_more': True}
        result = self.call('/api/messages', query={'thread_id': ['root'], 'after': ['0']}, method='GET')
        self.assertEqual(result['messages'], [])
        self.assertEqual(result['last_cursor'], '9223372036854775806')
        self.assertTrue(result['has_more'])
        self.assertTrue(self.backend.calls[0][1].endswith('limit=256&after=0'))
        self.backend.response = {'data': [], 'has_more': False}
        self.assertIsNone(self.call('/api/messages', query={'thread_id': ['root']}, method='GET')['first_cursor'])

    def test_invalid_mutations_perform_no_io(self):
        cases = [('/api/threads/create', {}), ('/api/threads/create', {'idempotency_key': 'ok', 'project_id': '../root'}),
                 ('/api/threads/create', {'idempotency_key': 'ok', 'title': ''}),
                 ('/api/turns/cancel', {'thread_id': '../root', 'turn_id': 't'}),
                 ('/api/turns/cancel', {'thread_id': 'root', 'turn_id': '..'}),
                 ('/api/turns/cancel', {'thread_id': 'root', 'turn_id': 't/a'}),
                 ('/api/turns/cancel', {'thread_id': 'root', 'turn_id': 't', 'extra': True}),
                 ('/api/turns/steer', {'thread_id': 'root', 'turn_id': 't', 'text': 'go'}),
                 ('/api/turns/steer', {'thread_id': 'root', 'turn_id': 't', 'text': '', 'message_id': 'm'})]
        for path, data in cases:
            with self.subTest(path=path, data=data), self.assertRaises(server.APIError):
                self.call(path, data)
        self.assertEqual(self.backend.calls, [])

    def test_invalid_pagination_performs_no_io(self):
        for extra in ({'before': ['0']}, {'before': ['']}, {'after': ['']}, {'after': ['-1']},
                      {'after': ['9223372036854775808']}, {'before': ['4'], 'after': ['0']},
                      {'limit': ['257']}, {'limit': ['1', '2']}, {'limit': ['0']}, {'cursor': ['2']}):
            with self.subTest(extra=extra), self.assertRaises(server.APIError):
                self.call('/api/messages', query={'thread_id': ['root'], **extra}, method='GET')
        self.assertEqual(self.backend.calls, [])

    def test_missing_auth_is_401_without_network(self):
        with tempfile.TemporaryDirectory() as tmp, patch.dict(os.environ, {'NANOCODEX_ACCOUNT_FILE': tmp + '/missing'}):
            backend = server.Backend()
            with patch.object(backend.opener, 'open') as opening:
                for path, data in (('/api/threads/create', {'idempotency_key': 'k'}), ('/api/turns/cancel', {'thread_id': 'root', 'turn_id': 't'})):
                    with self.assertRaises(server.APIError) as error:
                        backend.handle('POST', path, {}, data)
                    self.assertEqual(error.exception.status, 401)
                opening.assert_not_called()

    def test_loopback_new_routes_and_blank_query_validation(self):
        httpd = server.make_server(0, self.backend)
        thread = threading.Thread(target=httpd.serve_forever, daemon=True)
        thread.start()
        try:
            for path, method, data, headers, expected in (
                ('/api/turns/cancel', 'POST', {'thread_id': 'root', 'turn_id': 't'}, {'Origin': 'https://evil.invalid'}, 403),
                ('/api/messages?thread_id=root&after=', 'GET', None, {}, 400),
                ('/api/threads/archive', 'POST', {'thread_id': 'root'}, {}, 501),
                ('/api/turns/cancel', 'POST', {'thread_id': 'root', 'turn_id': 't'}, {}, 200)):
                conn = http.client.HTTPConnection('127.0.0.1', httpd.server_address[1])
                conn.request(method, path, body=json.dumps(data) if data is not None else None,
                             headers={'Content-Type': 'application/json', **headers})
                response = conn.getresponse()
                self.assertEqual(response.status, expected)
                response.read()
                conn.close()
        finally:
            httpd.shutdown()
            httpd.server_close()
            thread.join()
        self.assertEqual(len(self.backend.calls), 1)
