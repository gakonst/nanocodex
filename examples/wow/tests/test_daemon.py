"""Simulated local carrier only; no screen capture, wtype or account access."""
import json
from contextlib import closing
import sqlite3
from pathlib import Path
import tempfile
import threading
import time
import unittest

from transport.daemon import Bridge, Frame, Link, Assembler
from transport.messages import fragments


class Desktop:
    def __init__(self, session=17, request=None):
        self.responses = []
        self.assembler = Assembler(lambda kind, body: self.responses.append((kind, body)),
                                   accepted_kinds=('A', 'R', 'P', 'E', 'S'))
        self.peer = Link(session, self.assembler.receive)
        self.requests = list(fragments('Q', 1, request)) if request else []
        self.focus = self.allowed = True
        self.fail_input = False
        self.batches = []
        if self.requests:
            self.peer.send(self.requests.pop(0))

    def foreground(self):
        return self.focus

    def reserved(self):
        return self.allowed

    def capture(self):
        return self.peer.packet(ready=True)

    def send_keys(self, keys):
        self.batches.append(keys)
        if self.fail_input:
            return False
        digits = ''.join(str(int(k[1:]) - 13) for k in keys[1:-1])
        packet = bytes(int(digits[i:i+3], 8) for i in range(0, len(digits), 3))
        self.peer.receive(packet)
        if self.peer.pending is None and self.requests:
            self.peer.send(self.requests.pop(0))
        return True


class Dispatcher:
    def __init__(self):
        self.calls = []
        self.release = None
        self.entered = threading.Event()
        self.value = 'Ω reply ' * 200
        self.backend = self

    def handle(self, *args):
        return {'connected': False}

    def _read(self, rid):
        return ('fixture', {'done': True})

    def status(self):
        return {'connected': False}

    def dispatch(self, body, rid):
        self.entered.set()
        if self.release:
            self.release.wait(3)
        self.calls.append((body, rid))
        return [{'kind': 'reply', 'value': self.value, 'request_id': rid,
                 'state': 'reply', 'event_id': rid + ':reply'}]


class DaemonTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name) / 'bridge.sqlite3'
        self.request = json.dumps({'schemaVersion': 1, 'source': 'nanocodex-wow',
            'type': 'nanocodex.ask', 'mode': 'agent', 'prompt': '  Ω exact\n' * 30}).encode()
        self.desktop = Desktop(request=self.request)
        self.dispatcher = Dispatcher()
        self.bridge = Bridge(self.desktop, 17, self.dispatcher, self.path)
        self.addCleanup(lambda: self.bridge.close())

    def receive_request(self):
        for _ in range(100):
            self.bridge.step()
            if self.desktop.peer.pending is None:
                return
        self.fail('request was not ACKed')

    def test_pending_seq_one_and_stable_mapping_before_backend(self):
        self.assertEqual(self.desktop.peer.tx, 1)
        self.receive_request()
        self.assertEqual(self.dispatcher.calls, [])
        row = self.bridge.store.db.execute('SELECT session,mid,rid,body FROM bridge_requests').fetchone()
        self.assertEqual(row, (17, 1, 'ncw:00000011:0001', self.request))
        self.bridge.work_once()
        self.assertEqual(self.dispatcher.calls, [(self.request, row[2])])

    def test_mapping_committed_on_separate_connection_before_final_ack(self):
        emit = self.desktop.send_keys
        checked = []
        def inspect_before_emit(keys):
            # Final outbound request frame is still pending at the peer here.
            if not self.desktop.requests and self.desktop.peer.pending is not None:
                digits = ''.join(str(int(k[1:]) - 13) for k in keys[1:-1])
                packet = bytes(int(digits[i:i+3], 8) for i in range(0, len(digits), 3))
                if Frame.decode(packet).ack == self.desktop.peer.tx:
                    with closing(sqlite3.connect(self.path)) as db:
                        row = db.execute('SELECT session,mid,rid,body FROM bridge_requests').fetchone()
                        saved = json.loads(db.execute('SELECT body FROM bridge_state WHERE session=17').fetchone()[0])
                    self.assertEqual(row, (17, 1, 'ncw:00000011:0001', self.request))
                    self.assertEqual(saved['last_id'], 1)
                    self.assertEqual(self.dispatcher.calls, [])
                    self.assertFalse(self.bridge.stats['account_connected'])
                    checked.append(True)
            return emit(keys)
        self.desktop.send_keys = inspect_before_emit
        self.receive_request()
        self.assertEqual(checked, [True])

    def test_output_retained_until_final_ack_and_full_utf8(self):
        self.receive_request()
        self.bridge.work_once()
        self.bridge.step()
        self.assertEqual(self.bridge.store.db.execute('SELECT delivered FROM bridge_outputs').fetchone()[0], 0)
        for _ in range(200):
            self.bridge.step()
            if self.bridge.stats['delivered_outputs']:
                break
        self.assertEqual(self.desktop.responses, [('reply', self.dispatcher.value.encode())])
        self.assertEqual(self.bridge.stats['delivered_outputs'], 1)
        self.assertTrue(all(len(batch) <= 335 for batch in self.desktop.batches))
        self.assertFalse(self.bridge.evidence()['model_roundtrip_proven'])

    def test_stream_is_delivered_before_completion_and_empty_poll_keeps_waiting(self):
        self.receive_request()
        rid = 'ncw:00000011:0001'
        def output(kind, value, state, event):
            return dict(kind=kind, value=value, state=state, request_id=rid, event_id=rid + event)
        self.dispatcher.dispatch = lambda *args: [output('ack', 'queued', 'local_queued', ':queued')]
        responses = iter([
            [output('stream', 'ncs1\tr\tt\ts\t1\t0\tappend\nfirst incremental bytes', 'streaming', ':stream:1')],
            [],
            [output('stream', 'ncs1\tr\tt\ts\t2\t23\tdone\n', 'completed', ':stream:2')],
        ])
        self.dispatcher.poll = lambda *args: next(responses)
        self.bridge.work_once()
        self.bridge.work_once()
        for _ in range(80):
            self.bridge.step()
            if len(self.desktop.responses) == 2:
                break
        self.assertEqual(self.desktop.responses[-1], ('stream', b'ncs1\tr\tt\ts\t1\t0\tappend\nfirst incremental bytes'))
        self.assertEqual(self.bridge.store.db.execute('SELECT state FROM bridge_requests').fetchone()[0], 'waiting')
        self.bridge.work_once()  # No new events yet; this is not completion.
        self.assertEqual(self.bridge.store.db.execute('SELECT state FROM bridge_requests').fetchone()[0], 'waiting')
        self.bridge.work_once()
        self.assertEqual(self.bridge.store.db.execute('SELECT state FROM bridge_requests').fetchone()[0], 'done')

    def test_restart_partial_assembly_retains_identity_and_bytes(self):
        self.bridge.step()
        self.bridge.step()
        self.assertIsNotNone(self.bridge.assembler.current)
        self.bridge.close()
        self.bridge = Bridge(self.desktop, 17, self.dispatcher, self.path)
        self.receive_request()
        self.bridge.work_once()
        self.assertEqual(self.dispatcher.calls, [(self.request, 'ncw:00000011:0001')])

    def test_worker_backend_wait_does_not_block_carrier_loop(self):
        self.receive_request()
        self.dispatcher.release = threading.Event()
        self.bridge.start_worker()
        self.assertTrue(self.dispatcher.entered.wait(1))
        before = self.bridge.stats['captures']
        self.bridge.step()
        self.bridge.step()
        self.assertGreater(self.bridge.stats['captures'], before)
        self.dispatcher.release.set()

    def test_focus_and_reservation_block_input(self):
        self.desktop.focus = False
        self.assertFalse(self.bridge.step())
        self.desktop.focus = True
        self.desktop.allowed = False
        self.assertFalse(self.bridge.step())
        self.assertEqual(self.desktop.batches, [])

    def test_wrong_session_or_unowned_history_stops(self):
        self.desktop.peer.session = 18
        self.bridge.step()
        self.assertIsNotNone(self.bridge.error)
        self.assertEqual(self.desktop.batches, [])

    def test_ambiguous_input_persists_stop(self):
        self.desktop.fail_input = True
        self.bridge.step()
        self.bridge.step()
        self.assertIsNotNone(self.bridge.error)
        count = len(self.desktop.batches)
        self.bridge.close()
        self.bridge = Bridge(self.desktop, 17, self.dispatcher, self.path)
        self.bridge.step()
        self.assertEqual(len(self.desktop.batches), count)

    def test_stale_capture_never_authorizes_input(self):
        now = [0.0]
        self.bridge.clock = lambda: now[0]
        original = self.desktop.capture
        def slow():
            packet = original()
            now[0] += .6
            return packet
        self.desktop.capture = slow
        self.bridge.step()
        self.bridge.step()
        self.assertEqual(self.desktop.batches, [])

    def test_output_restart_keeps_wire_message_id_and_remaining_bytes(self):
        self.receive_request()
        self.bridge.work_once()
        self.bridge.step()
        self.bridge.close()
        self.bridge = Bridge(self.desktop, 17, self.dispatcher, self.path)
        for _ in range(200):
            self.bridge.step()
            if self.bridge.stats['delivered_outputs']:
                break
        self.assertEqual(self.desktop.responses, [('reply', self.dispatcher.value.encode())])
        self.assertEqual(len(self.dispatcher.calls), 1)

    def test_embedded_stable_id_is_retained(self):
        self.bridge.close()
        other = Path(self.temp.name) / 'other.sqlite3'
        body = json.dumps({'request_id': 'user-stable-id', 'prompt': 'exact'}).encode()
        self.desktop = Desktop(request=body)
        self.bridge = Bridge(self.desktop, 17, self.dispatcher, other)
        self.receive_request()
        self.bridge.work_once()
        self.assertEqual(self.dispatcher.calls, [(body, 'user-stable-id')])

    def test_evidence_omits_content(self):
        self.receive_request()
        text = json.dumps(self.bridge.evidence())
        self.assertNotIn('prompt', text)
        self.assertNotIn('exact', text)
        self.assertIn('packet_sha256', text)

    def test_capacity_refuses_final_request_ack_without_discard(self):
        from unittest.mock import patch
        with patch('transport.daemon.MAX_REQUESTS', 0):
            for _ in range(100):
                self.bridge.step()
                if self.bridge.error:
                    break
        self.assertIsNotNone(self.bridge.error)
        self.assertIsNotNone(self.desktop.peer.pending)
        self.assertLess(self.bridge.link.rx, self.desktop.peer.tx)
        self.assertEqual(self.dispatcher.calls, [])
        self.assertEqual(self.bridge.store.db.execute('SELECT count(*) FROM bridge_requests').fetchone()[0], 0)

    def test_second_daemon_cannot_own_journal(self):
        with self.assertRaises(BlockingIOError):
            Bridge(self.desktop, 17, self.dispatcher, self.path)


    def test_legacy_owner_rejected_before_sqlite_and_upgrade_can_write(self):
        import subprocess
        import sys
        from unittest.mock import patch
        from server import APIError
        self.bridge.close()
        self.bridge.close = lambda: None
        # An independent process models the old daemon's lifetime database flock.
        script = ("import fcntl,os,sys; "
                  "fd=os.open(sys.argv[1],os.O_RDONLY); "
                  "fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB); "
                  "print('locked',flush=True); sys.stdin.read(1)")
        legacy = subprocess.Popen([sys.executable, '-c', script, str(self.path)],
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        try:
            self.assertEqual(legacy.stdout.readline().strip(), 'locked')
            with patch('transport.daemon.EventStore') as store:
                with self.assertRaisesRegex(APIError, 'Legacy bridge'):
                    Bridge(self.desktop, 17, self.dispatcher, self.path)
                store.assert_not_called()
        finally:
            legacy.communicate('x', timeout=5)
        upgraded = Bridge(self.desktop, 17, self.dispatcher, self.path)
        try:
            with upgraded.store.db:
                upgraded.store.db.execute('CREATE TABLE upgrade_probe(value INTEGER)')
                upgraded.store.db.execute('INSERT INTO upgrade_probe VALUES(1)')
            self.assertEqual(upgraded.store.db.execute('SELECT value FROM upgrade_probe').fetchone(), (1,))
        finally:
            upgraded.close()

    def test_private_sidecar_lock_preserves_sqlite_writes_and_rejects_unsafe_files(self):
        from server import APIError
        lock = Path(str(self.path) + '.lock')
        self.assertEqual(lock.stat().st_mode & 0o777, 0o600)
        # SQLite transactions remain usable while the ownership lock is held.
        with self.bridge.store.db:
            self.bridge.store.db.execute("CREATE TABLE lock_probe(value INTEGER)")
            self.bridge.store.db.execute("INSERT INTO lock_probe VALUES(1)")
        self.assertEqual(self.bridge.store.db.execute("SELECT value FROM lock_probe").fetchone(), (1,))
        self.bridge.close()
        self.bridge.close = lambda: None
        lock.chmod(0o644)
        with self.assertRaises(APIError):
            Bridge(self.desktop, 17, self.dispatcher, self.path)
        lock.unlink()
        lock.symlink_to(self.path)
        with self.assertRaises(OSError):
            Bridge(self.desktop, 17, self.dispatcher, self.path)


class RealDispatcherTests(unittest.TestCase):
    def test_unknown_mutation_is_not_retried_after_reconnect(self):
        from transport.dispatch import Dispatcher as RealDispatcher
        class Backend:
            def __init__(self):
                self.calls = 0
            def handle(self, method, path, query, data):
                self.calls += 1
                raise ConnectionError('SECRET fixture exception')
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            backend = Backend()
            dispatcher = RealDispatcher(backend, path / 'dispatch.sqlite3')
            body = json.dumps(dict(schemaVersion=1, source='nanocodex-wow',
                type='nanocodex.action', action='create_project', name='Local fixture')).encode()
            desktop = Desktop(request=body)
            bridge = Bridge(desktop, 17, dispatcher, path / 'bridge.sqlite3')
            try:
                for _ in range(40):
                    bridge.step()
                    if desktop.peer.pending is None:
                        break
                bridge.work_once()
                bridge.close()
                bridge = Bridge(desktop, 17, dispatcher, path / 'bridge.sqlite3')
                bridge.work_once()
                self.assertEqual(backend.calls, 1)
                output = bridge.store.db.execute('SELECT body FROM bridge_outputs').fetchone()[0]
                self.assertNotIn('SECRET', output)
            finally:
                bridge.close()
                dispatcher.close()

    def test_real_status_error_reaches_addon_once(self):
        from transport.dispatch import Dispatcher as RealDispatcher
        from server import APIError
        class Backend:
            def handle(self, method, path, query, data):
                assert (method, path) == ('GET', '/api/status')
                raise APIError('SECRET fixture exception', 401)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            dispatcher = RealDispatcher(Backend(), path / 'dispatch.sqlite3')
            desktop = Desktop()
            bridge = Bridge(desktop, 17, dispatcher, path / 'bridge.sqlite3')
            try:
                bridge.check_status()
                bridge.check_status()
                for _ in range(20):
                    bridge.step()
                    if bridge.stats['delivered_outputs']:
                        break
                self.assertFalse(bridge.stats['account_connected'])
                self.assertEqual(len(desktop.responses), 1)
                self.assertEqual(desktop.responses[0][0], 'error')
                self.assertIn(b'Sign in', desktop.responses[0][1])
                self.assertNotIn(b'SECRET', desktop.responses[0][1])
                bridge.close()
                bridge = Bridge(desktop, 17, dispatcher, path / 'bridge.sqlite3')
                bridge.check_status()
                for _ in range(6):
                    bridge.step()
                self.assertEqual(len(desktop.responses), 1)
                self.assertEqual(bridge.store.db.execute('SELECT count(*) FROM bridge_outputs WHERE delivered=0').fetchone()[0], 0)
            finally:
                bridge.close()
                dispatcher.close()


class CalibrationCLITests(unittest.TestCase):
    def test_physical_xy_scale_forwarded_without_desktop_access(self):
        from types import SimpleNamespace
        from unittest.mock import patch
        from transport.daemon import main
        class StopBeforeDesktop(Exception):
            pass
        arguments = ['--window-address', 'fixture-address', '--window-class', 'fixture-class',
                     '--state-dir', '/unused-local-fixture', '--evidence', '/unused-local-fixture/evidence',
                     '--left', '0', '--top', '51', '--cell-size', '4.8',
                     '--cell-size-y', '4.67', '--output-scale', '2', '--session', '53281', '--allow-input']
        with patch('transport.daemon.desktop_session', return_value={'user':'fixture'}), \
             patch('os.geteuid', return_value=1000), \
             patch.dict('os.environ', WAYLAND_DISPLAY='local-fixture', HYPRLAND_INSTANCE_SIGNATURE='local-fixture'), \
             patch('transport.daemon.Desktop', side_effect=StopBeforeDesktop) as desktop:
            with self.assertRaises(StopBeforeDesktop):
                main(arguments)
            desktop.assert_called_once_with('fixture-address', 'fixture-class', 0, 51, 4.8,
                                            output_scale=2.0, cell_size_y=4.67, min_margin=2, input_backend="wayland", key_encoding="octal", key_hold_ms=0)


if __name__ == '__main__':
    unittest.main()
