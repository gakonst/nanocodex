"""Local TCP WebSocket fixtures; these tests don't contact Nanocodex."""
import json
import os
import socket
from pathlib import Path
import tempfile
import threading
import time
import unittest

from durable_client import EventStore, DurableClient, DurableBackend, ProtocolError, cursor
from unittest.mock import patch
from server import APIError
from websockets.sync.server import serve


class FixtureBackend:
    def __init__(self, port):
        self.base_url = f'http://127.0.0.1:{port}'

    def credentials(self):
        return 'synthetic-test-key'

    def request(self, *args):
        return {'agent_id': 'thread1', 'latest_event_cursor': '0'}


class DurableTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.path = Path(self.tmp.name) / 'state.sqlite3'
        self.store = EventStore(self.path)
        self.addCleanup(self.tmp.cleanup)
        self.addCleanup(lambda: self.store.close())

    def test_persist_deduplicate_and_ack(self):
        self.store.enqueue('thread1', 'turn1', 'hello')
        self.store.enqueue('thread1', 'turn1', 'hello')
        with self.assertRaises(APIError):
            self.store.enqueue('thread1', 'turn1', 'different')
        event = {'type': 'turn_accepted', 'id': 'turn1', 'cursor': '4'}
        self.store.accept('thread1', event)
        self.store.accept('thread1', event)
        self.assertEqual(self.store.pending('thread1'), [])
        self.assertEqual(len(self.store.events('thread1')), 1)
        self.store.close()
        self.store = EventStore(self.path)
        self.assertEqual(self.store.position('thread1'), '4')
        self.assertEqual(self.store.position('other'), '0')
        self.assertEqual(self.store.events('thread1', '4'), [])
        self.assertEqual(os.stat(self.path).st_mode & 0o777, 0o600)

    def test_atomic_cursor_on_write_failure(self):
        self.store.enqueue('thread1', 'turn1', 'hello')
        self.store.db.execute("CREATE TRIGGER fail_position BEFORE INSERT ON positions BEGIN SELECT RAISE(ABORT,'fixture'); END")
        with self.assertRaises(Exception):
            self.store.accept('thread1', {'type': 'turn_accepted', 'id': 'turn1', 'cursor': '1'})
        self.assertEqual(self.store.position('thread1'), '0')
        self.assertEqual(self.store.events('thread1'), [])
        self.assertEqual(len(self.store.pending('thread1')), 1)

    def test_cursor_validation(self):
        for value in ('latest', '-1', '01', '9223372036854775808', 1, None, '١'):
            with self.assertRaises(ProtocolError):
                cursor(value)
        self.assertEqual(cursor('9223372036854775807'), 9223372036854775807)

    def test_private_storage(self):
        unsafe = Path(self.tmp.name) / 'unsafe'
        unsafe.mkdir(mode=0o755)
        unsafe.chmod(0o755)
        with self.assertRaises(APIError):
            EventStore(unsafe / 'db')
        symlink = Path(self.tmp.name) / 'link'
        symlink.symlink_to(self.path)
        with self.assertRaises(OSError):
            EventStore(symlink)

    def server(self, handler):
        server = serve(handler, '127.0.0.1', 0)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        self.addCleanup(server.shutdown)
        backend = FixtureBackend(server.socket.getsockname()[1])
        client = DurableClient(backend, self.store, 'thread1', DurableClient.account_id(backend, backend.credentials()))
        self.addCleanup(client.close)
        return client

    def test_reconnect_reuses_intent_and_committed_cursor(self):
        paths, prompts, failures = [], [], []
        completed = threading.Event()
        def handler(ws):
            try:
                paths.append(ws.request.path)
                self.assertEqual(ws.request.headers['Authorization'], 'Bearer synthetic-test-key')
                self.assertEqual(ws.request.headers['User-Agent'], 'Nanocodex-WoW/0.1')
                ws.send(json.dumps({'type': 'ready', 'latest_event_cursor': '1'}))
                if len(paths) == 1:
                    with self.assertRaises(TimeoutError):
                        ws.recv(timeout=0.1)
                    self.assertEqual(self.store.position('thread1'), '0')
                    ws.send(json.dumps({'type': 'agent_created', 'cursor': '1'}))
                    prompts.append(json.loads(ws.recv(timeout=3)))
                    ws.close()
                else:
                    prompts.append(json.loads(ws.recv(timeout=3)))
                    ws.send(json.dumps({'type': 'turn_accepted', 'id': 'turn1', 'cursor': '2'}))
                    ws.send(json.dumps({'type': 'turn_completed', 'id': 'turn1', 'cursor': '3', 'final_message': 'ok'}))
                    deadline = time.monotonic() + 3
                    while self.store.position('thread1') != '3' and time.monotonic() < deadline:
                        time.sleep(0.01)
                    completed.set()
                    ws.recv(timeout=3)
            except Exception as exc:
                if not completed.is_set():
                    failures.append(type(exc).__name__)
                    completed.set()
        self.store.enqueue('thread1', 'turn1', 'hello')
        client = self.server(handler)
        client.start()
        self.assertTrue(completed.wait(8))
        self.assertEqual(failures, [])
        self.assertEqual(paths[:2], ['/v1/agents/thread1/ws?cursor=0', '/v1/agents/thread1/ws?cursor=1'])
        self.assertEqual(prompts, [{'type': 'prompt', 'id': 'turn1', 'input': 'hello'}] * 2)
        self.assertEqual(self.store.position('thread1'), '3')
        self.assertEqual(self.store.pending('thread1'), [])

    def test_uncorrelated_error_blocks_without_resubmit(self):
        seen = []
        def handler(ws):
            ws.send(json.dumps({'type': 'ready', 'latest_event_cursor': '0'}))
            seen.append(ws.recv(timeout=3))
            ws.send(json.dumps({'type': 'error', 'code': 'idempotency_conflict', 'message': 'fixture'}))
        self.store.enqueue('thread1', 'turn1', 'hello')
        client = self.server(handler)
        client.start()
        client.worker.join(timeout=4)
        self.assertEqual(client.status, 'blocked')
        self.assertEqual(len(seen), 1)
        self.assertEqual(len(self.store.pending('thread1')), 1)

    def test_receipt_without_cursor_does_not_advance(self):
        self.store.enqueue('thread1', 'turn1', 'hello')
        self.store.accept('thread1', {'type': 'turn_completed', 'id': 'turn1'})
        self.assertEqual(self.store.position('thread1'), '0')
        self.assertEqual(self.store.pending('thread1'), [])

    def test_send_route_queues_before_ack_and_requires_stable_key(self):
        backend = DurableBackend(storage_dir=Path(self.tmp.name) / 'backend')
        self.addCleanup(backend.close)
        with patch.object(backend, 'credentials', return_value='synthetic-test-key'), patch.object(DurableClient, 'start'):
            result = backend.handle('POST', '/api/send', {}, {'thread_id': 'thread1', 'text': 'hello', 'idempotency_key': 'turn1'})
            self.assertEqual(result['status'], 'queued')
            self.assertEqual(result['turn_id'], 'turn1')
            self.assertEqual(len(backend.store.pending('thread1')), 1)
            with self.assertRaises(APIError):
                backend.handle('POST', '/api/send', {}, {'thread_id': 'thread1', 'text': 'hello'})

    def test_idle_snapshot_forces_replay_without_adopting_cursor(self):
        paths = []
        done = threading.Event()
        def handler(ws):
            paths.append(ws.request.path)
            ws.send(json.dumps({'type': 'ready', 'latest_event_cursor': '0'}))
            if len(paths) > 1:
                ws.send(json.dumps({'type': 'event', 'cursor': '1', 'event': {}}))
                done.set()
            try:
                ws.recv(timeout=5)
            except Exception:
                pass
        client = self.server(handler)
        client.idle_interval = 0.1
        client.backend.request = lambda *args: {'agent_id': 'thread1', 'latest_event_cursor': '1'}
        client.start()
        self.assertTrue(done.wait(5))
        self.assertEqual(paths[:2], ['/v1/agents/thread1/ws?cursor=0'] * 2)

    def test_restart_restores_pending_threads(self):
        directory = Path(self.tmp.name) / 'resume'
        first = DurableBackend(storage_dir=directory)
        second = DurableBackend(storage_dir=directory)
        self.addCleanup(second.close)
        with patch.object(DurableBackend, 'credentials', return_value='synthetic-test-key'), patch.object(DurableClient, 'start'):
            first.subscribe('thread1')
            first.store.enqueue('thread1', 'turn1', 'hello')
            first.close()
            second.resume()
            self.assertIn('thread1', second.clients)
            self.assertEqual(len(second.store.pending('thread1')), 1)

    def test_temporary_dns_failure_reconnects(self):
        client = DurableClient(FixtureBackend(1), self.store, 'thread1', 'unused')
        calls = []
        def session():
            calls.append(1)
            if len(calls) == 1:
                raise socket.gaierror(-3, 'synthetic DNS failure')
            client.stop_event.set()
        with patch.object(client, 'session', side_effect=session), patch.object(client.stop_event, 'wait'):
            client.run()
        self.assertEqual(len(calls), 2)
        self.assertEqual(client.status, 'stopped')

    def test_account_change_blocks_before_network(self):
        client = DurableClient(FixtureBackend(1), self.store, 'thread1', 'wrong-account')
        client.run()
        self.assertEqual(client.status, 'blocked')


if __name__ == '__main__':
    unittest.main()
