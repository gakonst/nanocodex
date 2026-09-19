"""Hardware-free dispatcher tests; no live account or model requests."""
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from durable_client import DurableBackend, DurableClient
from server import APIError
from transport.messages import Assembler, fragments, MAX_MESSAGE
from transport.dispatch import (Dispatcher, InvalidRequest, encode_output, parse_request,
                                wire_fragments, MAX_VALUE)


def ask(**changes):
    value = {'schemaVersion': 1, 'source': 'nanocodex-wow', 'type': 'nanocodex.ask',
             'mode': 'agent', 'prompt': 'Hello', 'thread_id': 'thread1'}
    value.update(changes)
    return json.dumps(value)


def action(action_name, **fields):
    return json.dumps({'schemaVersion': 1, 'source': 'nanocodex-wow',
                      'type': 'nanocodex.action', 'action': action_name, **fields})


class FakeBackend:
    def __init__(self):
        self.calls = []
        self.failure = None
        self.send_status = 'queued'
        self.history = {'message_details': [], 'last_cursor': None}
        self.roster = [{'id': 'project1', 'name': 'A % | Ω'}]
        self.threads = [{'id': 'thread1', 'title': 'Chat name', 'status': 'unknown'}]

    def handle(self, method, path, query, data):
        self.calls.append((method, path, query, data))
        if self.failure:
            raise self.failure
        if path == '/api/send':
            return {'thread_id': 'thread1', 'turn_id': data['idempotency_key'], 'status': self.send_status}
        if path == '/api/messages':
            return self.history
        if path == '/api/projects':
            return {'projects': self.roster}
        if path == '/api/threads':
            return {'threads': self.threads}
        if path == '/api/status':
            return {'connected': False, 'error': 'private exception'}
        return {'status': 'created', 'project_id': 'p', 'thread_id': 't', 'name': data.get('name'), 'title': data.get('title')}


class DispatchTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name) / 'journal.db'
        self.backend = FakeBackend()
        self.d = Dispatcher(self.backend, self.path)
        self.addCleanup(lambda: self.d.close())

    def test_queue_is_not_remote_acceptance(self):
        out = self.d.dispatch(ask(context={'character': {'level': 60}}), 'stable.1')
        self.assertEqual(out[0]['state'], 'local_queued')
        method, path, query, data = self.backend.calls[0]
        self.assertEqual((method, path, query), ('POST', '/api/send', {}))
        self.assertEqual(data['idempotency_key'], 'stable.1')
        self.assertEqual(data['model'], 'luna')
        self.assertNotIn('reply', [x['kind'] for x in out])

    def test_remote_receipt_is_not_reply(self):
        self.backend.send_status = 'accepted'
        self.assertEqual(self.d.dispatch(ask(), 'r')[0]['state'], 'remoteaccepted')

    def test_missing_and_conflicting_ids(self):
        self.assertEqual(self.d.dispatch(ask())[0]['state'], 'error')
        self.assertEqual(self.d.dispatch(ask(request_id='other'), 'r')[0]['state'], 'error')
        self.assertEqual(self.backend.calls, [])
        self.assertEqual(self.d.dispatch(ask(request_id='r'))[0]['request_id'], 'r')

    def test_duplicate_replay_and_conflict_across_restart(self):
        first = self.d.dispatch(ask(), 'r')
        self.assertEqual(self.d.dispatch(ask(), 'r'), first)
        self.d.close()
        self.d = Dispatcher(self.backend, self.path)
        self.assertEqual(self.d.dispatch(ask(), 'r'), first)
        self.assertEqual(self.d.dispatch(ask(prompt='different'), 'r')[0]['value'], 'Request ID conflict.')
        self.assertEqual(len(self.backend.calls), 1)

    def test_unknown_mutation_never_retried(self):
        self.backend.failure = TimeoutError('Bearer DO_NOT_LEAK')
        out = self.d.dispatch(action('create_project', name='P'), 'r')
        self.assertEqual(out[0]['state'], 'unknown')
        self.assertNotIn('DO_NOT_LEAK', json.dumps(out))
        self.backend.failure = None
        self.assertEqual(self.d.dispatch(action('create_project', name='P'), 'r'), out)
        self.assertEqual(self.d.poll('r'), out)
        self.assertEqual(len(self.backend.calls), 1)

    def test_crash_after_intent_is_not_retried(self):
        self.backend.failure = KeyboardInterrupt()
        with self.assertRaises(KeyboardInterrupt):
            self.d.dispatch(ask(), 'r')
        self.d.close()
        self.d = Dispatcher(self.backend, self.path)
        self.backend.failure = None
        self.assertEqual(self.d.dispatch(ask(), 'r')[0]['state'], 'unknown')
        self.assertEqual(len(self.backend.calls), 1)

    def test_action_routes_exact(self):
        cases = [
            ('create_project', {'name': 'P'}, '/api/projects/create', {'name': 'P', 'idempotency_key': 'r0'}),
            ('create_chat', {'name': 'T', 'project_id': 'p'}, '/api/threads/create', {'title': 'T', 'project_id': 'p', 'idempotency_key': 'r1'}),
            ('rename_project', {'name': 'P', 'project_id': 'p'}, '/api/projects/rename', {'name': 'P', 'project_id': 'p'}),
            ('rename_chat', {'name': 'T', 'project_id': 'p', 'thread_id': 't'}, '/api/threads/rename', {'title': 'T', 'thread_id': 't'}),
        ]
        for i, (name, fields, route, data) in enumerate(cases):
            out = self.d.dispatch(action(name, **fields), f'r{i}')
            self.assertEqual(out[0]['state'], 'completed')
            self.assertEqual(self.backend.calls[-1], ('POST', route, {}, data))

    def test_strict_schema_and_no_route_forwarding(self):
        bad = [ask(path='/api/connect'), ask(api_key='secret'), ask(model='other'),
               ask(mode=[]), ask(schemaVersion=True), ask(source='other'),
               ask(prompt=' '), ask(thread_id='../x'), ask(context=[]),
               action('/api/turns/cancel'), action('rename_chat', name='T'),
               action('create_project', name='x', lua='print(1)'),
               action('create_project', name='bad\nname'), ask(prompt='x' * 16001),
               '[]', '{"schemaVersion":1,"schemaVersion":1}', '{"a":NaN}',
               b'\xff', ask(context={'x': 'x' * 65536}),
               ask(context={'x': [[[[[[[[[[[[[[0]]]]]]]]]]]]]]})]
        for payload in bad:
            with self.subTest(payload=str(payload)[:80]):
                self.assertEqual(self.d.dispatch(payload, 'bad')[0]['state'], 'error')
        self.assertEqual(self.backend.calls, [])

    def test_snapshot_percent_encoded_ncw1(self):
        out = self.d.projects('refresh')[0]
        self.assertEqual(out['kind'], 'projects')
        self.assertEqual(out['value'], 'ncw1\nP\tproject1\tA%20%25%20%7C%20%CE%A9\nT\tproject1\tthread1\tChat%20name\tunknown')
        self.assertEqual(encode_output(out), out['value'].encode())
        self.assertEqual(self.backend.calls[-1][2], {'project_id': ['project1']})

    def test_snapshot_rejects_controls_duplicates_and_limits(self):
        cases = [[{'id': 'p', 'name': 'bad\nname'}],
                 [{'id': 'p', 'name': 'P'}] * 2,
                 [{'id': 'p', 'name': 'P'}] * 1001]
        for i, roster in enumerate(cases):
            self.backend.roster = roster
            self.assertEqual(self.d.projects(f'r{i}')[0]['kind'], 'error')

    def test_no_raw_auth_errors(self):
        self.backend.failure = APIError('private key secret', 401)
        self.assertEqual(self.d.status(), {'connected': False, 'state': 'disconnected'})
        out = self.d.dispatch(ask(), 'r')
        self.assertEqual(out[0]['state'], 'error')
        self.assertNotIn('secret', json.dumps(out))

    def test_poll_matching_completion_only_and_dedup(self):
        self.d.dispatch(ask(), 'r')
        self.backend.history = {'message_details': [
            {'turn_id': 'other', 'event_type': 'turn_completed', 'text': 'wrong'},
            {'turn_id': 'r', 'event_type': 'event', 'text': 'partial'},
            {'turn_id': 'r', 'event_type': 'turn_accepted', 'text': 'user'}], 'last_cursor': '3'}
        first = self.d.poll('r')
        self.assertEqual([x['state'] for x in first], ['local_queued', 'remoteaccepted'])
        self.backend.history = {'message_details': [
            {'turn_id': 'r', 'event_type': 'turn_completed', 'text': 'Answer Ω'}], 'last_cursor': '4'}
        out = self.d.poll('r')
        self.assertEqual(out[-1]['kind'], 'reply')
        self.assertEqual(out[-1]['value'], 'Answer Ω')
        self.assertEqual(self.backend.calls[-1][2]['after'], ['3'])
        calls = len(self.backend.calls)
        self.assertEqual(self.d.poll('r'), out)
        self.assertEqual(len(self.backend.calls), calls)
        self.assertEqual(len({x['event_id'] for x in out}), len(out))

    def test_poll_failure_does_not_resubmit(self):
        self.d.dispatch(ask(), 'r')
        self.backend.failure = TimeoutError('secret')
        self.assertEqual(self.d.poll('r')[-1]['state'], 'unknown')
        self.assertEqual(sum(c[0] == 'POST' for c in self.backend.calls), 1)
        self.backend.failure = None
        self.assertEqual(self.d.poll('r')[0]['state'], 'local_queued')

    def test_oversized_reply_is_not_delivered(self):
        self.d.dispatch(ask(), 'r')
        self.backend.history = {'message_details': [{'turn_id': 'r', 'event_type': 'turn_completed', 'text': 'x' * (256 * 1024 + 1)}]}
        self.assertNotIn('reply', [x['kind'] for x in self.d.poll('r')])

    def test_capacity_fails_closed_without_evicting(self):
        with patch('transport.dispatch.MAX_RECORDS', 1):
            out = self.d.dispatch(ask(), 'r')
            self.assertEqual(self.d.dispatch(ask(), 'other')[0]['state'], 'error')
            self.assertEqual(self.d.dispatch(ask(), 'r'), out)
        self.assertEqual(len(self.backend.calls), 1)

    def test_real_durable_backend_enqueue_and_terminal_failure(self):
        backend = DurableBackend(storage_dir=Path(self.temp.name) / 'durable')
        self.addCleanup(backend.close)
        self.d.backend = backend
        with patch.object(backend, 'credentials', return_value='synthetic-test-key'), patch.object(DurableClient, 'start'):
            self.assertEqual(self.d.dispatch(ask(), 'r')[0]['state'], 'local_queued')
            self.assertEqual(backend.store.pending('thread1')[0][0], 'r')
            backend.store.accept('thread1', {'type': 'turn_failed', 'id': 'r', 'cursor': '1'})
            with patch.object(backend, 'request', return_value={'data': [], 'has_more': False}):
                self.assertEqual(self.d.poll('r')[-1]['state'], 'error')
            self.assertEqual(backend.store.pending('thread1'), [])

    def test_durable_reply_offline_survives_restart(self):
        backend = DurableBackend(storage_dir=Path(self.temp.name) / 'durable')
        self.addCleanup(backend.close)
        self.d.backend = backend
        with patch.object(backend, 'credentials', return_value='synthetic-test-key'), patch.object(DurableClient, 'start'):
            self.d.dispatch(ask(), 'r')
            backend.store.accept('thread1', {'type': 'turn_accepted', 'id': 'r', 'cursor': '1'})
            backend.store.accept('thread1', {'type': 'turn_completed', 'id': 'r', 'cursor': '2', 'final_message': 'Offline reply'})
            with patch.object(backend, 'request', side_effect=AssertionError('No network needed')):
                out = self.d.poll('r')
            self.assertEqual([x['state'] for x in out], ['local_queued', 'remoteaccepted', 'streaming', 'streaming', 'completed'])
            self.d.close()
            self.d = Dispatcher(backend, self.path)
            self.assertEqual(self.d.poll('r'), out)

    def test_malformed_backend_receipts_are_not_success(self):
        with patch.object(self.backend, 'handle', return_value={}):
            self.assertEqual(self.d.dispatch(action('create_project', name='P'), 'p')[0]['state'], 'unknown')
            self.assertEqual(self.d.dispatch(ask(), 'r')[0]['state'], 'unknown')

    def test_real_backend_missing_auth_is_not_success(self):
        backend = DurableBackend(storage_dir=Path(self.temp.name) / 'durable')
        self.addCleanup(backend.close)
        self.d.backend = backend
        with patch.object(backend, 'credentials', side_effect=APIError('Missing auth', 401)):
            self.assertFalse(self.d.status()['connected'])
            self.assertEqual(self.d.dispatch(ask(), 'r')[0]['state'], 'error')
            self.assertIsNone(backend.store)

    def complete(self, text, rid='r'):
        self.d.dispatch(ask(), rid)
        self.backend.history = {'message_details': [
            {'turn_id': rid, 'event_type': 'turn_completed', 'text': text}], 'last_cursor': '1'}
        return self.d.poll(rid)

    def wire_roundtrip(self, output, expected_kind):
        delivered = []
        receiver = Assembler(lambda kind, body: delivered.append((kind, body)),
                             accepted_kinds=('R', 'P', 'A', 'E'))
        chunks = list(wire_fragments(output, 1))
        for chunk in chunks[:-1]:
            self.assertLessEqual(len(chunk), 96)
            receiver.receive(chunk)
        self.assertEqual(delivered, [])  # Native UI sees no partial content.
        receiver.receive(chunks[-1])
        self.assertEqual(delivered, [(expected_kind, output['value'].encode())])
        return delivered[0][1].decode('utf-8')

    def test_full_reply_utf8_and_restart_stable(self):
        text = 'Hello Ω 🎮 ' * 800
        outputs = self.complete(text)
        self.assertEqual(outputs[-1]['value'], text)
        self.assertEqual(self.wire_roundtrip(outputs[-1], 'reply'), text)
        calls = len(self.backend.calls)
        self.d.close()
        self.d = Dispatcher(self.backend, self.path)
        self.assertEqual(self.d.poll('r'), outputs)
        self.assertEqual(len(self.backend.calls), calls)
        self.assertNotIn('more', outputs[-1])

    def test_full_16k_reply_plain_bytes_without_json_escaping(self):
        text = '\x00\n"\\' * 4096
        output = self.complete(text, rid='r' * 128)[-1]
        self.assertEqual(len(encode_output(output)), MAX_MESSAGE)
        self.assertEqual(self.wire_roundtrip(output, 'reply'), text)

    def test_complete_ncw1_snapshot_single_message(self):
        self.backend.threads = [{'id': 't' + str(i), 'title': 'Long chat title Ω ' * 3, 'status': 'unknown'} for i in range(12)]
        output = self.d.projects('roster')[0]
        snapshot = self.wire_roundtrip(output, 'projects')
        self.assertTrue(snapshot.startswith('ncw1\nP\tproject1\t'))
        self.assertEqual(len(snapshot.splitlines()), 14)
        self.assertIn('T\tproject1\tt11\t', snapshot)
        self.assertGreater(len(snapshot.encode()), 128)
        self.assertNotIn('more', output)

    def test_oversized_snapshot_emits_error_not_partial_ncw1(self):
        self.backend.threads = [{'id': 't' + str(i), 'title': 'Ω' * 160, 'status': 'unknown'} for i in range(100)]
        outputs = self.d.projects('roster')
        self.assertEqual([x['kind'] for x in outputs], ['error'])
        self.wire_roundtrip(outputs[0], 'error')

    def test_wire_encoding_rejects_oversize_without_truncation(self):
        output = self.complete('valid')[-1]
        with self.assertRaises(InvalidRequest):
            list(wire_fragments({**output, 'value': 'x' * (MAX_MESSAGE + 1)}, 1))

    def test_retention_limit_is_explicit_terminal_error(self):
        out = self.complete('x' * (256 * 1024 + 1))
        self.assertEqual(out[-1]['kind'], 'error')
        self.assertIn('display limit', out[-1]['value'])
        calls = len(self.backend.calls)
        self.assertEqual(self.d.poll('r'), out)
        self.assertEqual(len(self.backend.calls), calls)
        self.assertNotIn('reply', [x['kind'] for x in out])

    def test_serialized_request_limit_exact_boundary_and_one_byte_over(self):
        payload = json.dumps({'schemaVersion': 1, 'source': 'nanocodex-wow',
            'type': 'nanocodex.ask', 'request_id': 'boundary', 'mode': 'hint',
            'prompt': 'hello', 'context': ''}, separators=(',', ':'))
        payload = payload.replace('"context":""', '"context":"' + 'x' * (MAX_MESSAGE - len(payload.encode())) + '"')
        self.assertEqual(len(payload.encode()), MAX_MESSAGE)
        self.assertEqual(self.d.dispatch(payload)[0]['state'], 'local_queued')
        self.assertEqual(len(self.backend.calls), 1)
        for too_large in (payload + ' ', (payload + ' ').encode()):
            error = self.d.dispatch(too_large, 'oversize')[0]
            self.assertEqual(error['kind'], 'error')
            self.assertIn('16 KiB', error['value'])
            self.assertLessEqual(len(encode_output(error)), MAX_MESSAGE)
        self.assertEqual(len(self.backend.calls), 1)

    def test_escaping_and_utf8_count_toward_request_limit(self):
        value = json.loads(ask(prompt='Ω' * 9000))
        for payload in (json.dumps(value, ensure_ascii=False), json.dumps(value)):
            self.assertGreater(len(payload.encode()), MAX_MESSAGE)
            self.assertEqual(self.d.dispatch(payload, 'large')[0]['kind'], 'error')
        self.assertEqual(self.backend.calls, [])

    def test_codec_complete_bytes_exact_prompt_and_final_ack_is_only_delivery(self):
        prompt = '  preserve whitespace\r\n' + '🎮Ω' * 80 + ' \t'
        data = json.loads(ask(prompt=prompt, request_id='stable-key'))
        payload = json.dumps(data, ensure_ascii=False).encode()
        pieces = list(fragments('Q', 1, payload))
        # Verify that this fixture really splits a multibyte codepoint.
        split_utf8 = False
        for piece in pieces:
            try:
                piece[8:].decode('utf-8')
            except UnicodeDecodeError:
                split_utf8 = True
        self.assertTrue(split_utf8)
        deliveries, receipts = [], []
        def deliver(kind, complete):
            self.assertEqual(kind, 'request')
            deliveries.append(complete)
            receipts.extend(self.d.dispatch(complete))
            return True
        assembler = Assembler(deliver, accepted_kinds=('Q',))
        for piece in pieces[:-1]:
            self.assertTrue(assembler.receive(piece))
        self.assertEqual(self.backend.calls, [])
        self.assertTrue(assembler.receive(pieces[-1]))
        self.assertEqual(deliveries, [payload])
        self.assertEqual(receipts[0]['state'], 'local_queued')
        self.assertNotIn('reply', [r['kind'] for r in receipts])
        self.assertEqual(self.backend.calls[0][3]['text'], prompt)
        self.assertEqual(self.backend.calls[0][3]['idempotency_key'], 'stable-key')
        # Fresh carrier session after uncertain final ACK preserves application ID.
        retry = Assembler(deliver, accepted_kinds=('Q',))
        for piece in pieces:
            retry.receive(piece)
        self.assertEqual(len(self.backend.calls), 1)
        self.assertEqual(receipts[0], receipts[1])

    def test_codec_plain_error_and_full_reply_roundtrip(self):
        outputs = self.complete('Ω🎮' * 120)
        page = outputs[-1]
        delivered = []
        receiver = Assembler(lambda kind, body: delivered.append((kind, body)),
                             accepted_kinds=('R', 'E'))
        for chunk in fragments('R', 1, page['value']):
            receiver.receive(chunk)
        error = self.d.dispatch(b'x' * (MAX_MESSAGE + 1), 'oversized')[0]
        for chunk in fragments('E', 2, error['value']):
            receiver.receive(chunk)
        self.assertEqual(delivered, [('reply', page['value'].encode()),
                                     ('error', error['value'].encode())])
        self.assertEqual(page['value'], 'Ω🎮' * 120)

    def test_output_is_data_and_bounded(self):
        out = self.d.dispatch(ask(prompt='loadstring("x")'), 'r')[0]
        self.assertEqual(encode_output(out), out['value'].encode())
        self.wire_roundtrip(out, 'ack')
        self.assertEqual(self.backend.calls[0][3]['text'], 'loadstring("x")')
        with self.assertRaises(InvalidRequest):
            encode_output({'kind': 'lua', 'value': 'x'})


if __name__ == '__main__':
    unittest.main()
